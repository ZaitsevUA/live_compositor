use std::sync::Arc;

use h264_reader::nal::{
    pps::PicParameterSet,
    slice::{
        DecRefPicMarking, MemoryManagementControlOperation, ModificationOfPicNums, NumRefIdxActive,
        RefPicListModifications, SliceHeader,
    },
    sps::SeqParameterSet,
};

use super::{
    DecodeInformation, DecoderInstruction, PictureInfo, ReferencePictureInfo, Slice, SpsExt,
};

#[derive(Debug, thiserror::Error)]
pub enum ReferenceManagementError {
    #[error("B frames are not supported")]
    BFramesNotSupported,

    #[error("SI frames are not supported")]
    SIFramesNotSupported,

    #[error("SP frames are not supported")]
    SPFramesNotSupported,

    #[error("PicOrderCntType {0} is not supperted")]
    PicOrderCntTypeNotSupported(u8),

    #[error("The H.264 bytestream is not spec compliant: {0}.")]
    IncorrectData(String),
}

#[derive(Debug, Default, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReferenceId(usize);

#[derive(Debug, Default)]
#[allow(non_snake_case)]
pub(crate) struct ReferenceContext {
    pictures: ReferencePictures,
    next_reference_id: ReferenceId,
    previous_frame_num: usize,
    prev_pic_order_cnt_msb: i32,
    prev_pic_order_cnt_lsb: i32,
    MaxLongTermFrameIdx: MaxLongTermFrameIdx,
    prevFrameNumOffset: i64,
    previous_picture_included_mmco_equal_5: bool,
    current_picture_included_mmco_equal_5: bool,
}

#[derive(Debug, Default)]
enum MaxLongTermFrameIdx {
    #[default]
    NoLongTermFrameIndices,
    Idx(u64),
}

impl ReferenceContext {
    fn get_next_reference_id(&mut self) -> ReferenceId {
        let result = self.next_reference_id;
        self.next_reference_id = ReferenceId(result.0 + 1);
        result
    }

    fn reset_state(&mut self) {
        *self = Self {
            pictures: ReferencePictures::default(),
            next_reference_id: ReferenceId::default(),
            previous_frame_num: 0,
            prev_pic_order_cnt_msb: 0,
            prev_pic_order_cnt_lsb: 0,
            MaxLongTermFrameIdx: MaxLongTermFrameIdx::NoLongTermFrameIndices,
            prevFrameNumOffset: 0,
            previous_picture_included_mmco_equal_5: false,
            current_picture_included_mmco_equal_5: false,
        };
    }

    #[allow(non_snake_case)]
    fn add_long_term_reference(
        &mut self,
        header: Arc<SliceHeader>,
        LongTermFrameIdx: u64,
        pic_order_cnt: [i32; 2],
    ) -> ReferenceId {
        let id = self.get_next_reference_id();
        self.pictures.long_term.push(LongTermReferencePicture {
            header,
            id,
            LongTermFrameIdx,
            pic_order_cnt,
        });

        id
    }

    fn add_short_term_reference(
        &mut self,
        header: Arc<SliceHeader>,
        pic_order_cnt: [i32; 2],
    ) -> ReferenceId {
        let id = self.get_next_reference_id();
        self.pictures.short_term.push(ShortTermReferencePicture {
            header,
            id,
            pic_order_cnt,
        });
        id
    }

    pub(crate) fn put_picture(
        &mut self,
        mut slices: Vec<Slice>,
        sps: &SeqParameterSet,
        pps: &PicParameterSet,
    ) -> Result<Vec<DecoderInstruction>, ReferenceManagementError> {
        let header = slices.last().unwrap().header.clone();

        // maybe this should be done in a different place, but if you think about it, there's not
        // really that many places to put this code in
        let mut rbsp_bytes = Vec::new();
        let mut slice_indices = Vec::new();
        for slice in &mut slices {
            if slice.rbsp_bytes.is_empty() {
                continue;
            }
            slice_indices.push(rbsp_bytes.len());
            rbsp_bytes.append(&mut slice.rbsp_bytes);
        }

        let decode_info =
            self.decode_information_for_frame(header.clone(), slice_indices, rbsp_bytes, sps, pps)?;

        let decoder_instructions = match &header.clone().dec_ref_pic_marking {
            Some(DecRefPicMarking::Idr {
                long_term_reference_flag,
                ..
            }) => self.reference_picture_marking_process_idr(
                header,
                decode_info,
                *long_term_reference_flag,
            )?,

            Some(DecRefPicMarking::SlidingWindow) => {
                self.reference_picture_marking_process_sliding_window(sps, header, decode_info)?
            }
            Some(DecRefPicMarking::Adaptive(memory_management_control_operations)) => self
                .reference_picture_marking_process_adaptive(
                    sps,
                    header,
                    decode_info,
                    memory_management_control_operations,
                )?,

            // this picture is not a reference
            None => vec![DecoderInstruction::Decode { decode_info }],
        };

        self.previous_picture_included_mmco_equal_5 = self.current_picture_included_mmco_equal_5;
        self.current_picture_included_mmco_equal_5 = false;

        Ok(decoder_instructions)
    }

    fn remove_long_term_ref(
        &mut self,
        long_term_frame_idx: u64,
    ) -> Result<LongTermReferencePicture, ReferenceManagementError> {
        for (i, frame) in self.pictures.long_term.iter().enumerate() {
            if frame.LongTermFrameIdx == long_term_frame_idx {
                return Ok(self.pictures.long_term.remove(i));
            }
        }

        Err(ReferenceManagementError::IncorrectData(
            format!("cannot remove long term reference with id {long_term_frame_idx}, because it does not exist")
        ))
    }

    #[allow(non_snake_case)]
    fn remove_short_term_ref(
        &mut self,
        current_frame_num: i64,
        sps: &SeqParameterSet,
        pic_num_to_remove: i64,
    ) -> Result<ShortTermReferencePicture, ReferenceManagementError> {
        for (i, picture) in self.pictures.short_term.iter().enumerate() {
            let PicNum = decode_picture_numbers_for_short_term_ref(
                picture.header.frame_num.into(),
                current_frame_num,
                sps,
            )
            .PicNum;

            if PicNum == pic_num_to_remove {
                return Ok(self.pictures.short_term.remove(i));
            }
        }

        Err(ReferenceManagementError::IncorrectData(
            format!("cannot remove long term reference with pic num {pic_num_to_remove}, because it does not exist")
        ))
    }

    fn reference_picture_marking_process_adaptive(
        &mut self,
        sps: &SeqParameterSet,
        header: Arc<SliceHeader>,
        decode_info: DecodeInformation,
        memory_management_control_operations: &[MemoryManagementControlOperation],
    ) -> Result<Vec<DecoderInstruction>, ReferenceManagementError> {
        let mut decoder_instructions = Vec::new();

        let mut new_long_term_frame_idx = None;

        for memory_management_control_operation in memory_management_control_operations {
            match memory_management_control_operation {
                MemoryManagementControlOperation::ShortTermUnusedForRef {
                    difference_of_pic_nums_minus1,
                } => {
                    let pic_num_to_remove =
                        header.frame_num as i64 - (*difference_of_pic_nums_minus1 as i64 + 1);

                    let removed = self.remove_short_term_ref(
                        header.frame_num.into(),
                        sps,
                        pic_num_to_remove,
                    )?;

                    decoder_instructions.push(DecoderInstruction::Drop {
                        reference_ids: vec![removed.id],
                    });
                }

                MemoryManagementControlOperation::LongTermUnusedForRef { long_term_pic_num } => {
                    let removed = self.remove_long_term_ref(*long_term_pic_num as u64)?;

                    decoder_instructions.push(DecoderInstruction::Drop {
                        reference_ids: vec![removed.id],
                    });
                }

                MemoryManagementControlOperation::ShortTermUsedForLongTerm {
                    difference_of_pic_nums_minus1,
                    long_term_frame_idx,
                } => {
                    if let Ok(removed) = self.remove_long_term_ref(*long_term_frame_idx as u64) {
                        decoder_instructions.push(DecoderInstruction::Drop {
                            reference_ids: vec![removed.id],
                        });
                    }

                    let pic_num_to_remove =
                        header.frame_num as i64 - (*difference_of_pic_nums_minus1 as i64 + 1);

                    let picture = self.remove_short_term_ref(
                        header.frame_num.into(),
                        sps,
                        pic_num_to_remove,
                    )?;

                    self.pictures.long_term.push(LongTermReferencePicture {
                        header: picture.header,
                        LongTermFrameIdx: *long_term_frame_idx as u64,
                        pic_order_cnt: picture.pic_order_cnt,
                        id: picture.id,
                    });
                }

                MemoryManagementControlOperation::MaxUsedLongTermFrameRef {
                    max_long_term_frame_idx_plus1,
                } => {
                    if *max_long_term_frame_idx_plus1 != 0 {
                        self.MaxLongTermFrameIdx =
                            MaxLongTermFrameIdx::Idx(*max_long_term_frame_idx_plus1 as u64 - 1);
                    } else {
                        self.MaxLongTermFrameIdx = MaxLongTermFrameIdx::NoLongTermFrameIndices;
                    }

                    let max_idx = *max_long_term_frame_idx_plus1 as i128 - 1;

                    let reference_ids_to_remove = self
                        .pictures
                        .long_term
                        .iter()
                        .filter(|p| p.LongTermFrameIdx as i128 > max_idx)
                        .map(|p| p.id)
                        .collect();

                    self.pictures.long_term = self
                        .pictures
                        .long_term
                        .iter()
                        .filter(|p| p.LongTermFrameIdx as i128 <= max_idx)
                        .cloned()
                        .collect();

                    decoder_instructions.push(DecoderInstruction::Drop {
                        reference_ids: reference_ids_to_remove,
                    })
                }

                MemoryManagementControlOperation::AllRefPicturesUnused => {
                    let reference_ids = self
                        .pictures
                        .short_term
                        .drain(..)
                        .map(|p| p.id)
                        .chain(self.pictures.long_term.drain(..).map(|p| p.id))
                        .collect();

                    self.MaxLongTermFrameIdx = MaxLongTermFrameIdx::NoLongTermFrameIndices;
                    self.current_picture_included_mmco_equal_5 = true;

                    decoder_instructions.push(DecoderInstruction::Drop { reference_ids })
                }
                MemoryManagementControlOperation::CurrentUsedForLongTerm {
                    long_term_frame_idx,
                } => {
                    if let Ok(picture) = self.remove_long_term_ref(*long_term_frame_idx as u64) {
                        decoder_instructions.push(DecoderInstruction::Drop {
                            reference_ids: vec![picture.id],
                        });
                    }

                    new_long_term_frame_idx = Some(*long_term_frame_idx as u64);
                }
            }
        }

        let reference_id = match new_long_term_frame_idx {
            Some(long_term_frame_idx) => self.add_long_term_reference(
                header,
                long_term_frame_idx,
                decode_info.picture_info.PicOrderCnt,
            ),
            None => self.add_short_term_reference(header, decode_info.picture_info.PicOrderCnt),
        };

        decoder_instructions.insert(
            0,
            DecoderInstruction::DecodeAndStoreAs {
                decode_info,
                reference_id,
            },
        );

        if let MaxLongTermFrameIdx::Idx(max) = self.MaxLongTermFrameIdx {
            if self.pictures.long_term.len() > max as usize + 1 {
                return Err(ReferenceManagementError::IncorrectData(format!(
                    "there are {} long-term references, but there shouldn't be more than {max}",
                    self.pictures.long_term.len()
                )));
            }
        }

        Ok(decoder_instructions)
    }

    fn reference_picture_marking_process_sliding_window(
        &mut self,
        sps: &SeqParameterSet,
        header: Arc<SliceHeader>,
        decode_info: DecodeInformation,
    ) -> Result<Vec<DecoderInstruction>, ReferenceManagementError> {
        let num_short_term = self.pictures.short_term.len();
        let num_long_term = self.pictures.long_term.len();

        let reference_id =
            self.add_short_term_reference(header.clone(), decode_info.picture_info.PicOrderCnt);

        let mut decoder_instructions = vec![DecoderInstruction::DecodeAndStoreAs {
            decode_info,
            reference_id,
        }];

        if num_short_term + num_long_term == sps.max_num_ref_frames.max(1) as usize
            && !self.pictures.short_term.is_empty()
        {
            let (idx, _) = self
                .pictures
                .short_term
                .iter()
                .enumerate()
                .min_by_key(|(_, reference)| {
                    decode_picture_numbers_for_short_term_ref(
                        reference.header.frame_num.into(),
                        header.frame_num.into(),
                        sps,
                    )
                    .FrameNumWrap
                })
                .unwrap();

            decoder_instructions.push(DecoderInstruction::Drop {
                reference_ids: vec![self.pictures.short_term.remove(idx).id],
            })
        }

        Ok(decoder_instructions)
    }

    fn reference_picture_marking_process_idr(
        &mut self,
        header: Arc<SliceHeader>,
        decode_info: DecodeInformation,
        long_term_reference_flag: bool,
    ) -> Result<Vec<DecoderInstruction>, ReferenceManagementError> {
        self.reset_state();

        let reference_id = if long_term_reference_flag {
            self.MaxLongTermFrameIdx = MaxLongTermFrameIdx::Idx(0);
            self.add_long_term_reference(header, 0, decode_info.picture_info.PicOrderCnt)
        } else {
            self.MaxLongTermFrameIdx = MaxLongTermFrameIdx::NoLongTermFrameIndices;
            self.add_short_term_reference(header, decode_info.picture_info.PicOrderCnt)
        };

        Ok(vec![DecoderInstruction::Idr {
            decode_info,
            reference_id,
        }])
    }

    fn decode_information_for_frame(
        &mut self,
        header: Arc<SliceHeader>,
        slice_indices: Vec<usize>,
        rbsp_bytes: Vec<u8>,
        sps: &SeqParameterSet,
        pps: &PicParameterSet,
    ) -> Result<DecodeInformation, ReferenceManagementError> {
        let reference_list = match header.slice_type.family {
            h264_reader::nal::slice::SliceFamily::P => {
                let num_ref_idx_l0_active = header
                    .num_ref_idx_active
                    .as_ref()
                    .map(|num| match num {
                        NumRefIdxActive::P {
                            num_ref_idx_l0_active_minus1,
                        } => Ok(*num_ref_idx_l0_active_minus1),
                        NumRefIdxActive::B { .. } => {
                            Err(ReferenceManagementError::BFramesNotSupported)
                        }
                    })
                    .unwrap_or(Ok(pps.num_ref_idx_l0_default_active_minus1))?
                    + 1;

                let mut reference_list =
                    self.initialize_reference_picture_list_for_frame(&header, sps)?;

                match &header.ref_pic_list_modification {
                    Some(RefPicListModifications::P {
                        ref_pic_list_modification_l0,
                    }) => {
                        self.modify_reference_picture_list(
                            sps,
                            &header,
                            &mut reference_list,
                            ref_pic_list_modification_l0,
                        )?;
                    }

                    None
                    | Some(RefPicListModifications::I)
                    | Some(RefPicListModifications::B { .. }) => return Err(ReferenceManagementError::IncorrectData(
                        "a slice marked 'P' slice family contains a reference picture list for a different family".into()
                    ))?,
                }

                reference_list.truncate(num_ref_idx_l0_active as usize);

                Some(reference_list)
            }
            h264_reader::nal::slice::SliceFamily::I => None,
            h264_reader::nal::slice::SliceFamily::B => {
                return Err(ReferenceManagementError::BFramesNotSupported)?
            }
            h264_reader::nal::slice::SliceFamily::SP => {
                return Err(ReferenceManagementError::SPFramesNotSupported)?
            }
            h264_reader::nal::slice::SliceFamily::SI => {
                return Err(ReferenceManagementError::SIFramesNotSupported)?
            }
        };

        let pic_order_cnt = match sps.pic_order_cnt {
            h264_reader::nal::sps::PicOrderCntType::TypeZero {
                log2_max_pic_order_cnt_lsb_minus4,
            } => self.decode_pic_order_cnt_type_zero(&header, log2_max_pic_order_cnt_lsb_minus4)?,

            h264_reader::nal::sps::PicOrderCntType::TypeOne { .. } => {
                Err(ReferenceManagementError::PicOrderCntTypeNotSupported(1))?
            }

            h264_reader::nal::sps::PicOrderCntType::TypeTwo => {
                self.decode_pic_order_cnt_type_two(&header, sps)?
            }
        };

        Ok(DecodeInformation {
            reference_list,
            header: header.clone(),
            slice_indices,
            rbsp_bytes,
            sps_id: sps.id().id(),
            pps_id: pps.pic_parameter_set_id.id(),
            picture_info: PictureInfo {
                non_existing: false,
                used_for_long_term_reference: false,
                PicOrderCnt: pic_order_cnt,
                FrameNum: header.frame_num,
            },
        })
    }

    #[allow(non_snake_case)]
    fn decode_pic_order_cnt_type_two(
        &mut self,
        header: &SliceHeader,
        sps: &SeqParameterSet,
    ) -> Result<[i32; 2], ReferenceManagementError> {
        let FrameNumOffset = if header.idr_pic_id.is_some() {
            0
        } else {
            let prevFrameNumOffset = if self.previous_picture_included_mmco_equal_5 {
                0
            } else {
                self.prevFrameNumOffset
            };

            if self.previous_frame_num > header.frame_num.into() {
                prevFrameNumOffset + sps.max_frame_num()
            } else {
                prevFrameNumOffset
            }
        };

        let tempPicOrderCnt = if header.idr_pic_id.is_some() {
            0
        } else if header.dec_ref_pic_marking.is_none() {
            2 * (FrameNumOffset as i32 + header.frame_num as i32) - 1
        } else {
            2 * (FrameNumOffset as i32 + header.frame_num as i32)
        };

        self.prevFrameNumOffset = FrameNumOffset;

        Ok([tempPicOrderCnt; 2])
    }

    fn decode_pic_order_cnt_type_zero(
        &mut self,
        header: &SliceHeader,
        log2_max_pic_order_cnt_lsb_minus4: u8,
    ) -> Result<[i32; 2], ReferenceManagementError> {
        // this section is very hard to read, but all of this code is just copied from the
        // h.264 spec, where it looks almost exactly like this

        let max_pic_order_cnt_lsb = 2_i32.pow(log2_max_pic_order_cnt_lsb_minus4 as u32 + 4);

        let (prev_pic_order_cnt_msb, prev_pic_order_cnt_lsb) = if header.idr_pic_id.is_some() {
            (0, 0)
        } else {
            (self.prev_pic_order_cnt_msb, self.prev_pic_order_cnt_lsb)
        };

        let (pic_order_cnt_lsb, delta_pic_order_cnt_bottom) = match header
                    .pic_order_cnt_lsb
                    .as_ref()
                    .ok_or(ReferenceManagementError::IncorrectData("pic_order_cnt_lsb is not present in a slice header, but is required for decoding".into()))?
                {
                    h264_reader::nal::slice::PicOrderCountLsb::Frame(pic_order_cnt_lsb) => {
                        (*pic_order_cnt_lsb, 0)
                    }
                    h264_reader::nal::slice::PicOrderCountLsb::FieldsAbsolute {
                        pic_order_cnt_lsb,
                        delta_pic_order_cnt_bottom,
                    } => (*pic_order_cnt_lsb, *delta_pic_order_cnt_bottom),
                    h264_reader::nal::slice::PicOrderCountLsb::FieldsDelta(_) => {
                        Err(ReferenceManagementError::IncorrectData("pic_order_cnt_lsb is not present in a slice header, but is required for decoding".into()))?
                    }
                };

        let pic_order_cnt_lsb = pic_order_cnt_lsb as i32;

        let pic_order_cnt_msb = if pic_order_cnt_lsb < prev_pic_order_cnt_lsb
            && prev_pic_order_cnt_lsb - pic_order_cnt_lsb >= max_pic_order_cnt_lsb / 2
        {
            prev_pic_order_cnt_msb + max_pic_order_cnt_lsb
        } else if pic_order_cnt_lsb > prev_pic_order_cnt_lsb
            && pic_order_cnt_lsb - prev_pic_order_cnt_lsb > max_pic_order_cnt_lsb / 2
        {
            prev_pic_order_cnt_msb - max_pic_order_cnt_lsb
        } else {
            prev_pic_order_cnt_msb
        };

        let pic_order_cnt = if header.field_pic == h264_reader::nal::slice::FieldPic::Frame {
            let top_field_order_cnt = pic_order_cnt_msb + pic_order_cnt_lsb;

            let bottom_field_order_cnt = top_field_order_cnt + delta_pic_order_cnt_bottom;

            top_field_order_cnt.min(bottom_field_order_cnt)
        } else {
            pic_order_cnt_msb + pic_order_cnt_lsb
        };

        self.prev_pic_order_cnt_msb = pic_order_cnt_msb;
        self.prev_pic_order_cnt_lsb = pic_order_cnt_lsb;

        Ok([pic_order_cnt; 2])
    }

    fn initialize_short_term_reference_picture_list_for_frame(
        &self,
        header: &SliceHeader,
        sps: &SeqParameterSet,
    ) -> Vec<ReferencePictureInfo> {
        let mut short_term_reference_list = self
            .pictures
            .short_term
            .iter()
            .map(|reference| {
                (
                    reference,
                    decode_picture_numbers_for_short_term_ref(
                        reference.header.frame_num.into(),
                        header.frame_num.into(),
                        sps,
                    ),
                )
            })
            .collect::<Vec<_>>();

        short_term_reference_list.sort_by_key(|(_, numbers)| -numbers.PicNum);

        short_term_reference_list
            .into_iter()
            .map(|(reference, numbers)| ReferencePictureInfo {
                id: reference.id,
                LongTermPicNum: None,
                picture_info: PictureInfo {
                    FrameNum: numbers.FrameNum as u16,
                    used_for_long_term_reference: false,
                    non_existing: false,
                    PicOrderCnt: reference.pic_order_cnt,
                },
            })
            .collect()
    }

    fn initialize_long_term_reference_picture_list_for_frame(&self) -> Vec<ReferencePictureInfo> {
        let mut long_term_reference_list = self.pictures.long_term.clone();

        long_term_reference_list.sort_by_key(|pic| pic.LongTermFrameIdx);

        long_term_reference_list
            .into_iter()
            .map(|pic| ReferencePictureInfo {
                id: pic.id,
                LongTermPicNum: Some(pic.LongTermFrameIdx),
                picture_info: PictureInfo {
                    used_for_long_term_reference: true,
                    non_existing: false,
                    FrameNum: pic.header.frame_num,
                    PicOrderCnt: pic.pic_order_cnt,
                },
            })
            .collect()
    }

    fn initialize_reference_picture_list_for_frame(
        &self,
        header: &SliceHeader,
        sps: &SeqParameterSet,
    ) -> Result<Vec<ReferencePictureInfo>, ReferenceManagementError> {
        let short_term_reference_list =
            self.initialize_short_term_reference_picture_list_for_frame(header, sps);

        let long_term_reference_list = self.initialize_long_term_reference_picture_list_for_frame();

        let reference_list = short_term_reference_list
            .into_iter()
            .chain(long_term_reference_list)
            .collect::<Vec<_>>();

        Ok(reference_list)
    }

    #[allow(non_snake_case)]
    fn modify_reference_picture_list(
        &self,
        sps: &SeqParameterSet,
        header: &SliceHeader,
        reference_list: &mut Vec<ReferencePictureInfo>,
        ref_pic_list_modifications: &[ModificationOfPicNums],
    ) -> Result<(), ReferenceManagementError> {
        // 0 is Subtract, 1 is Add, 2 is LongTermRef
        let mut refIdxL0 = 0;
        let mut picNumL0Pred = header.frame_num as i64;

        for ref_pic_list_modification in ref_pic_list_modifications {
            match ref_pic_list_modification {
                ModificationOfPicNums::Subtract(_) | ModificationOfPicNums::Add(_) => {
                    self.modify_short_term_reference_picture_list(
                        sps,
                        header,
                        reference_list,
                        ref_pic_list_modification,
                        &mut refIdxL0,
                        &mut picNumL0Pred,
                    )?;
                }

                ModificationOfPicNums::LongTermRef(long_term_pic_num) => {
                    self.modify_long_term_reference_picture_list(
                        reference_list,
                        *long_term_pic_num,
                        &mut refIdxL0,
                    )?;
                }
            }
        }

        Ok(())
    }

    #[allow(non_snake_case)]
    fn modify_long_term_reference_picture_list(
        &self,
        reference_list: &mut Vec<ReferencePictureInfo>,
        picture_to_shift: u32,
        refIdxLX: &mut usize,
    ) -> Result<(), ReferenceManagementError> {
        let shifted_picture_idx = reference_list
            .iter()
            .enumerate()
            .find(|(_, pic)| match pic.LongTermPicNum {
                Some(num) => num == picture_to_shift.into(),
                None => false,
            })
            .map(|(i, _)| i)
            .ok_or(ReferenceManagementError::IncorrectData(
                format!("picture with LongTermPicNum = {picture_to_shift} is not present in the reference list during modification")
            ))?;

        if reference_list[shifted_picture_idx]
            .picture_info
            .non_existing
        {
            return Err(ReferenceManagementError::IncorrectData(
                "a short-term reference picture marked for shifting in the reference list modification process is marked as non-existing".into()
            ));
        }

        if !reference_list[shifted_picture_idx]
            .picture_info
            .used_for_long_term_reference
        {
            return Err(ReferenceManagementError::IncorrectData(
                "a short-term reference picture marked for shifting in the long-term reference list modification process".into()
            ));
        }

        let shifted_picture = reference_list.remove(shifted_picture_idx);
        reference_list.insert(*refIdxLX, shifted_picture);
        *refIdxLX += 1;

        Ok(())
    }

    #[allow(non_snake_case)]
    fn modify_short_term_reference_picture_list(
        &self,
        sps: &SeqParameterSet,
        header: &SliceHeader,
        reference_list: &mut Vec<ReferencePictureInfo>,
        ref_pic_list_modification: &ModificationOfPicNums,
        refIdxLX: &mut usize,
        picNumLXPred: &mut i64,
    ) -> Result<(), ReferenceManagementError> {
        let picNumLXNoWrap = match *ref_pic_list_modification {
            ModificationOfPicNums::Subtract(abs_diff_pic_num_minus_1) => {
                let abs_diff_pic_num = abs_diff_pic_num_minus_1 as i64 + 1;
                if *picNumLXPred - abs_diff_pic_num < 0 {
                    *picNumLXPred - abs_diff_pic_num + sps.max_frame_num()
                } else {
                    *picNumLXPred - abs_diff_pic_num
                }
            }
            ModificationOfPicNums::Add(abs_diff_pic_num_minus_1) => {
                let abs_diff_pic_num = abs_diff_pic_num_minus_1 as i64 + 1;
                if *picNumLXPred + abs_diff_pic_num >= sps.max_frame_num() {
                    *picNumLXPred + abs_diff_pic_num - sps.max_frame_num()
                } else {
                    *picNumLXPred + abs_diff_pic_num
                }
            }
            ModificationOfPicNums::LongTermRef(_) => return Ok(()),
        };

        *picNumLXPred = picNumLXNoWrap;

        let picNumLX = if picNumLXNoWrap > header.frame_num as i64 {
            picNumLXNoWrap - sps.max_frame_num()
        } else {
            picNumLXNoWrap
        };

        let shifted_picture_idx = reference_list
            .iter()
            .enumerate()
            .find(|(_, picture_info)| decode_picture_numbers_for_short_term_ref(picture_info.picture_info.FrameNum.into(), header.frame_num.into(), sps).PicNum == picNumLX)
            .map(|(i, _)| i)
            .ok_or(ReferenceManagementError::IncorrectData(
                format!("picture with picNumLX = {picNumLX} is not present in the reference list during modification")
            ))?;

        if reference_list[shifted_picture_idx]
            .picture_info
            .non_existing
        {
            return Err(ReferenceManagementError::IncorrectData(
                "a short-term reference picture marked for shifting in the reference list modification process is marked as non-existing".into()
            ));
        }

        if reference_list[shifted_picture_idx]
            .picture_info
            .used_for_long_term_reference
        {
            return Err(ReferenceManagementError::IncorrectData(
                "a long-term reference picture marked for shifting in the short-term reference list modification process".into()
            ));
        }

        let shifted_picture_info = reference_list.remove(shifted_picture_idx);
        reference_list.insert(*refIdxLX, shifted_picture_info);
        *refIdxLX += 1;

        Ok(())
    }
}

#[derive(Debug)]
struct ShortTermReferencePicture {
    header: Arc<SliceHeader>,
    id: ReferenceId,
    pic_order_cnt: [i32; 2],
}

#[allow(non_snake_case)]
fn decode_picture_numbers_for_short_term_ref(
    frame_num: i64,
    current_frame_num: i64,
    sps: &SeqParameterSet,
) -> ShortTermReferencePictureNumbers {
    let MaxFrameNum = sps.max_frame_num();

    let FrameNum = frame_num;

    let FrameNumWrap = if FrameNum > current_frame_num {
        FrameNum - MaxFrameNum
    } else {
        FrameNum
    };

    // this assumes we're dealing with a short-term reference frame
    let PicNum = FrameNumWrap;

    ShortTermReferencePictureNumbers {
        FrameNum,
        FrameNumWrap,
        PicNum,
    }
}

#[derive(Debug, Clone)]
#[allow(non_snake_case)]
struct LongTermReferencePicture {
    header: Arc<SliceHeader>,
    LongTermFrameIdx: u64,
    id: ReferenceId,
    pic_order_cnt: [i32; 2],
}

#[allow(non_snake_case)]
struct ShortTermReferencePictureNumbers {
    FrameNum: i64,

    FrameNumWrap: i64,

    PicNum: i64,
}

#[derive(Debug, Default)]
struct ReferencePictures {
    long_term: Vec<LongTermReferencePicture>,
    short_term: Vec<ShortTermReferencePicture>,
}