import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

// vitePreprocess kvůli `<script lang="ts">`: Svelte 5 sám zvládne jen
// typové anotace, ne zbytek TypeScriptu (enumy, `satisfies` v šablonách…).
export default {
	preprocess: vitePreprocess()
};
