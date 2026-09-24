import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [svelte()],
	// Tauri dev čeká na pevné adrese (tauri.conf.json → devUrl). Kdyby
	// Vite při obsazeném portu tiše přeskočil na jiný, okno aplikace by
	// zůstalo prázdné a nikdo by nevěděl proč — proto strictPort.
	clearScreen: false,
	server: {
		port: 1420,
		strictPort: true,
		// Změny v Rustu hlídá Tauri CLI; Vite by jen zbytečně
		// přenačítal stránku při každém buildu backendu.
		watch: { ignored: ['**/src-tauri/**'] }
	},
	build: {
		// WebView2 je Chromium, které se aktualizuje samo — není
		// důvod překládat moderní JS do starého.
		target: 'chrome105',
		// Mapy zdrojů by jen nafoukly binárku (frontend je v ní vestavěný).
		sourcemap: false
	}
});
