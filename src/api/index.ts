// 统一 API 入口:在 Tauri 中调用真实后端命令,在浏览器中回退到 mock。
// 判定依据:Tauri 会注入 window.__TAURI__。当前后端尚未实现,统一走 mock。

import { mockApi } from './mock';
import { tauriApi } from './tauri';

// Tauri 2 默认不注入全局 __TAURI__,但一定会注入 __TAURI_INTERNALS__
export const isTauri =
  typeof window !== 'undefined' &&
  ('__TAURI_INTERNALS__' in window || '__TAURI__' in window);

// 在 Tauri 中调用真实后端;在浏览器中回退到 mock。
export const api = isTauri ? tauriApi : mockApi;

export * from './types';
