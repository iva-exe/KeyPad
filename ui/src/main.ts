import './app.css';
// Jen latinka a latinka-ext (čeština): vietnamské řezy by jen zbytečně
// nafoukly binárku, do které se frontend vestavuje. Písma (a vůbec
// všechno) jsou vlastní soubory, žádné CDN: CSP v tauri.conf.json
// pustí jen 'self' a IPC — cizí adresa nebo data: URI by v releasu
// potichu nenačetly (ve vývoji CSP neplatí, takže to tam neuvidíš).
import '@fontsource/space-grotesk/latin-400.css';
import '@fontsource/space-grotesk/latin-ext-400.css';
import '@fontsource/space-grotesk/latin-500.css';
import '@fontsource/space-grotesk/latin-ext-500.css';
import '@fontsource/space-grotesk/latin-600.css';
import '@fontsource/space-grotesk/latin-ext-600.css';
import '@fontsource/fira-mono/latin-400.css';
import '@fontsource/fira-mono/latin-ext-400.css';
import '@fontsource/fira-mono/latin-500.css';
import '@fontsource/fira-mono/latin-ext-500.css';

import { mount } from 'svelte';
import App from './App.svelte';

// Výchozí kontextové menu WebView2 (Zpět, Obnovit, Uložit jako, Tisk…)
// v nástroji nemá co dělat a „Obnovit" by jen zbytečně bliklo oknem.
// Ve vývoji zůstává — „Prozkoumat" se hodí.
if (import.meta.env.PROD) {
	document.addEventListener('contextmenu', (e) => e.preventDefault());
}

/**
 * Počká na písma, než se poprvé vykreslí.
 *
 * @fontsource má `font-display: swap`: první snímek by se nakreslil
 * náhradním písmem a o pár milisekund později přeskočil na Space
 * Grotesk — text by se v okně viditelně cuknul. Písma jsou vestavěná
 * v binárce, takže načtení trvá milisekundy; pojistka 400 ms je jen
 * pro případ, že by se některé nenačetlo vůbec (okno se ukáže tak jako
 * tak, jen s náhradním písmem).
 */
async function pockejNaPisma(): Promise<void> {
	if (!('fonts' in document)) return;
	// Vzorek s diakritikou, ať se stáhne i latin-ext podmnožina.
	const vzorek = 'KeyPad ěščřžýáíéůú';
	const rezy = [
		'400 1em "Space Grotesk"',
		'500 1em "Space Grotesk"',
		'600 1em "Space Grotesk"',
		'400 1em "Fira Mono"'
	];
	await Promise.race([
		Promise.all(rezy.map((r) => document.fonts.load(r, vzorek))),
		new Promise((hotovo) => setTimeout(hotovo, 400))
	]);
}

const cil = document.getElementById('app');
if (!cil) throw new Error('chybí #app v index.html');

void pockejNaPisma()
	.catch(() => undefined)
	.then(() => mount(App, { target: cil }));
