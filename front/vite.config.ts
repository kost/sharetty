import { defineConfig } from 'vite';
import { resolve } from 'path';

export default defineConfig({
    build: {
        outDir: '../static',
        emptyOutDir: false, // Don't delete other static files
        lib: {
            entry: resolve(__dirname, 'src/main.ts'),
            name: 'sharetty',
            fileName: (format) => `js/sharetty.js`,
            formats: ['iife'], // Immediately Invoked Function Expression for browser script tag
        },
        rollupOptions: {
            output: {
                entryFileNames: 'js/sharetty.js',
                assetFileNames: (assetInfo) => {
                    if (assetInfo.name && assetInfo.name.endsWith('.css')) return 'css/sharetty.css';
                    return 'assets/[name][extname]';
                },
            },
        },
    },
});
