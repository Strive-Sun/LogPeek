import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Tauri 期望固定端口;浏览器开发直接用同一端口
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
});
