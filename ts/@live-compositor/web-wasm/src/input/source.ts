import type { RegisterInputRequest } from '@live-compositor/core';
import MP4Source from './mp4/source';
import type { Framerate } from '../compositor';

export type VideoChunk = {
  data: EncodedVideoChunk;
  ptsMs: number;
};

export type SourcePayload = { type: 'chunk'; chunk: EncodedVideoChunk } | { type: 'eos' };

export type InputSourceCallbacks = {
  onDecoderConfig: (config: VideoDecoderConfig) => void;
};

export default interface InputSource {
  init(): Promise<void>;
  /**
   * Starts input processing. `init()` has to be called beforehand.
   */
  start(): void;
  registerCallbacks(callbacks: InputSourceCallbacks): void;
  /**
   * if `true` InputSource won't produce more chunks anymore
   */
  isFinished(): boolean;
  getFramerate(): Framerate | undefined;
  nextChunk(): VideoChunk | undefined;
  peekChunk(): VideoChunk | undefined;
}

export function sourceFromRequest(request: RegisterInputRequest): InputSource {
  if (request.type === 'mp4') {
    return new MP4Source(request.url!);
  } else {
    throw new Error(`Unknown input type ${(request as any).type}`);
  }
}
