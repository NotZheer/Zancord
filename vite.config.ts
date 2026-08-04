import { defineConfig } from 'vite';
import fs from 'fs';
import path from 'path';

const hasCert = fs.existsSync('./key.pem') && fs.existsSync('./cert.pem');

export default defineConfig({
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: '0.0.0.0',
    https: hasCert
      ? {
          key: fs.readFileSync(path.resolve(__dirname, 'key.pem')),
          cert: fs.readFileSync(path.resolve(__dirname, 'cert.pem')),
        }
      : false,
    proxy: {
      '/socket.io': {
        target: hasCert ? 'https://localhost:3443' : 'http://localhost:3000',
        ws: true,
        secure: false,
      },
    },
  },
  envPrefix: ['VITE_'],
  build: {
    target: ['es2021', 'chrome100', 'safari14'],
    minify: 'esbuild',
    sourcemap: false,
  },
});
