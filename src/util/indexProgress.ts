import type { IndexProgress } from '../api/types';

type ProgressSubscriber = (progress: IndexProgress) => boolean | void;

/**
 * Replays the latest progress event to late subscribers. Terminal events stay
 * available until the owning session is explicitly cleared.
 */
export class IndexProgressStore {
  private readonly latest = new Map<string, IndexProgress>();
  private readonly subscribers = new Map<string, Set<ProgressSubscriber>>();

  publish(progress: IndexProgress): void {
    this.latest.set(progress.sessionId, progress);
    const subscribers = this.subscribers.get(progress.sessionId);
    if (!subscribers) return;
    for (const subscriber of subscribers) {
      if (subscriber(progress) === false) subscribers.delete(subscriber);
    }
    if (subscribers.size === 0) this.subscribers.delete(progress.sessionId);
  }

  subscribe(sessionId: string, subscriber: ProgressSubscriber): () => void {
    const subscribers = this.subscribers.get(sessionId) ?? new Set<ProgressSubscriber>();
    subscribers.add(subscriber);
    this.subscribers.set(sessionId, subscribers);

    const latest = this.latest.get(sessionId);
    if (latest && subscriber(latest) === false) {
      subscribers.delete(subscriber);
      if (subscribers.size === 0) this.subscribers.delete(sessionId);
    }

    return () => {
      const current = this.subscribers.get(sessionId);
      current?.delete(subscriber);
      if (current?.size === 0) this.subscribers.delete(sessionId);
    };
  }

  getLatest(sessionId: string): IndexProgress | undefined {
    return this.latest.get(sessionId);
  }

  clear(sessionId: string): void {
    this.latest.delete(sessionId);
    this.subscribers.delete(sessionId);
  }
}
