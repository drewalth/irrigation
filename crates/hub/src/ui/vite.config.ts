import { defineConfig } from 'vite'
import preact from '@preact/preset-vite'
import { viteSingleFile } from "vite-plugin-singlefile"
import path from "path"
import tailwindcss from "@tailwindcss/vite"

// https://vite.dev/config/
export default defineConfig({
  plugins: [preact(), tailwindcss(), viteSingleFile()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
})
