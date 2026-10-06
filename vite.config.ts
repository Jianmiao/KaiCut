import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'node:path'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    host: '127.0.0.1', port: 5173, strictPort: true,
    watch: { ignored: ['**/src-tauri/**', '**/macos-for-shadcn/**', '**/apple-web-design-skill/**', '**/fu-liquiglass-skill/**', '**/PingFangSC/**'] },
  },
  clearScreen: false,
  resolve: {
    alias: {
      '@': path.resolve(process.cwd(), './src'),
    },
  },
})
