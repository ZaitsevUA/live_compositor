import type { InputId } from '@live-compositor/browser-render';
import { CompositorEventType } from 'live-compositor';
import type { EventSender } from '../eventSender';
import type InputSource from './source';
import { Queue } from '@datastructures-js/queue';
import { H264Decoder } from './decoder/h264Decoder';
import { FrameRef } from './frame';
import { assert, framerateToDurationMs } from '../utils';

export type InputState = 'waiting_for_start' | 'buffering' | 'playing' | 'finished';

const MAX_BUFFERING_SIZE = 3;

export class Input {
  private id: InputId;
  private state: InputState;
  private source: InputSource;
  private decoder: H264Decoder;
  private eventSender: EventSender;
  private frames: Queue<FrameRef>;
  /**
   * Queue PTS of the first frame
   */
  private startPtsMs?: number;

  public constructor(id: InputId, source: InputSource, eventSender: EventSender) {
    this.id = id;
    this.state = 'waiting_for_start';
    this.source = source;
    this.eventSender = eventSender;
    this.frames = new Queue();
    this.decoder = new H264Decoder({
      onFrame: frame => {
        this.frames.push(new FrameRef(frame));
      },
    });

    this.source.registerCallbacks({
      onDecoderConfig: config => this.decoder.configure(config),
    });
  }

  public start() {
    if (this.state !== 'waiting_for_start') {
      console.warn(`Tried to start an already started input "${this.id}"`);
      return;
    }

    this.source.start();
    this.state = 'buffering';
    this.eventSender.sendEvent({
      type: CompositorEventType.VIDEO_INPUT_DELIVERED,
      inputId: this.id,
    });
  }

  public async getFrameRef(currentQueuePts: number): Promise<FrameRef | undefined> {
    if (this.state === 'buffering') {
      this.handleBuffering();
      return;
    }
    if (this.state !== 'playing') {
      return;
    }
    if (this.startPtsMs === undefined) {
      this.startPtsMs = currentQueuePts;
    }

    this.dropOldFrames(currentQueuePts);
    this.enqueueChunks(currentQueuePts);

    // No more chunks will be produced. Flush all the remaining frames from the decoder
    if (this.source.isFinished() && this.decoder.decodeQueueSize() !== 0) {
      await this.decoder.flush();
    }

    let frame: FrameRef | undefined;
    if (this.source.isFinished() && this.frames.size() == 1) {
      // Last frame is not poped by `dropOldFrames`
      frame = this.frames.pop();
    } else {
      frame = this.getLatestFrame();
    }

    if (frame) {
      return frame;
    }

    // Source received EOS & there is no more frames
    if (this.source.isFinished()) {
      this.handleEos();
      return;
    }

    return undefined;
  }

  /**
   * Retrieves latest frame and increments its reference count
   */
  private getLatestFrame(): FrameRef | undefined {
    const frame = this.frames.front();
    if (frame) {
      frame.incrementRefCount();
      return frame;
    }

    return undefined;
  }

  /**
   * Finds frame with PTS closest to `currentQueuePts` and removes frames older than it
   */
  private dropOldFrames(currentQueuePts: number): void {
    if (this.frames.isEmpty()) {
      return;
    }

    const frames = this.frames.toArray();
    const targetPts = this.queuePtsToInputPts(currentQueuePts);

    const targetFrame = frames.reduce((prevFrame, frame) => {
      const prevPtsDiff = Math.abs(prevFrame.getPtsMs() - targetPts);
      const currPtsDiff = Math.abs(frame.getPtsMs() - targetPts);
      return prevPtsDiff < currPtsDiff ? prevFrame : frame;
    });

    for (const frame of frames) {
      if (frame.getPtsMs() < targetFrame.getPtsMs()) {
        frame.decrementRefCount();
        this.frames.pop();
      }
    }
  }

  private handleBuffering() {
    if (this.frames.size() < MAX_BUFFERING_SIZE) {
      this.tryEnqueueChunk();
      return;
    }

    this.state = 'playing';
    this.eventSender.sendEvent({
      type: CompositorEventType.VIDEO_INPUT_PLAYING,
      inputId: this.id,
    });
  }

  private handleEos() {
    this.state = 'finished';
    this.eventSender.sendEvent({
      type: CompositorEventType.VIDEO_INPUT_EOS,
      inputId: this.id,
    });

    this.decoder.close();
  }

  private queuePtsToInputPts(queuePts: number): number {
    assert(this.startPtsMs !== undefined);
    return queuePts - this.startPtsMs;
  }

  private tryEnqueueChunk() {
    const chunk = this.source.nextChunk();
    if (chunk) {
      this.decoder.decode(chunk.data);
    }
  }

  private enqueueChunks(currentQueuePts: number) {
    const framrate = this.source.getFramerate();
    assert(framrate);

    const frameDuration = framerateToDurationMs(framrate);
    const targetPts = this.queuePtsToInputPts(currentQueuePts) + frameDuration * MAX_BUFFERING_SIZE;

    let chunk = this.source.peekChunk();
    while (chunk && chunk.ptsMs < targetPts) {
      this.decoder.decode(chunk.data);
      this.source.nextChunk();
      chunk = this.source.peekChunk();
    }
  }
}
