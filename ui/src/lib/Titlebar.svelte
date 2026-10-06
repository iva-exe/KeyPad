<script lang="ts">
	import Info from 'lucide-svelte/icons/info';
	import Minus from 'lucide-svelte/icons/minus';
	import X from 'lucide-svelte/icons/x';
	import { aplikace } from './aplikace.svelte';
	import OAplikaci from './OAplikaci.svelte';
	import { hlavniOkno } from './tauri';

	// Titulek: jméno, ⓘ (nastavení a o aplikaci), minimalizovat, zavřít. Stav
	// ovladače je v jeho kartě — dvakrát na jedné obrazovce by jen
	// přidával text.

	let oAplikaci = $state(false);
	let tlacitkoInfo: HTMLButtonElement | undefined = $state();

	// Křížek jen schovává, když je kam (ikona v oznamovací oblasti).
	// Bez ikony aplikaci ukončí — a bublina to nesmí tvrdit jinak.
	const zavrit = $derived(
		aplikace.tray === true
			? 'Schovat do oznamovací oblasti'
			: aplikace.tray === false
				? 'Ukončit KeyPad'
				: 'Zavřít'
	);
</script>

<!-- „deep": táhne se za celý pruh včetně loga; tlačítka tažení
     blokují sama (Tauri je pozná jako klikatelné prvky). Dvojklik okno
     nemaximalizuje — v tauri.conf.json je maximizable: false, protože
     nástroj přes celou obrazovku nedává smysl. -->
<header class="titlebar" data-tauri-drag-region="deep">
	<div class="brand">
		<img src="/icon.png" alt="" width="18" height="18" draggable="false" />
		<span class="wordmark">KeyPad</span>
	</div>

	<div class="win-controls">
		<button
			class="wc"
			class:aktivni={oAplikaci}
			title="Nastavení a o aplikaci"
			aria-label="Nastavení a o aplikaci"
			aria-expanded={oAplikaci}
			bind:this={tlacitkoInfo}
			onclick={() => (oAplikaci = !oAplikaci)}
		>
			<Info size={16} strokeWidth={1.75} />
		</button>
		<button
			class="wc"
			title="Minimalizovat"
			aria-label="Minimalizovat"
			onclick={() => void hlavniOkno()?.minimize()}
		>
			<Minus size={17} strokeWidth={1.75} />
		</button>
		<!-- Zavření okno jen schová (backend, on_window_event): zapnutý
		     ovladač zůstává zapnutý. Ukončit jde z ikony v oznamovací
		     oblasti. -->
		<button class="wc close" title={zavrit} aria-label={zavrit} onclick={() => void hlavniOkno()?.close()}>
			<X size={18} strokeWidth={1.75} />
		</button>
	</div>
</header>

<!-- Mimo titulek: v něm by každý klik do panelu začal tahat okno. -->
{#if oAplikaci}
	<OAplikaci kotva={tlacitkoInfo} onzavri={() => (oAplikaci = false)} />
{/if}

<style>
	.titlebar {
		display: flex;
		align-items: center;
		gap: 1.1rem;
		height: 44px;
		padding: 0 0.4rem 0 0.9rem;
		flex-shrink: 0;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--accent);
	}
	.brand img {
		display: block;
	}
	.wordmark {
		font-weight: 600;
		font-size: 0.98rem;
		letter-spacing: 0.01em;
	}
	.win-controls {
		margin-left: auto;
		display: flex;
		align-items: center;
	}
	.wc {
		display: grid;
		place-items: center;
		width: 40px;
		height: 32px;
		padding: 0;
		border: 0;
		border-radius: var(--radius);
		background: transparent;
		color: var(--text-dim);
		cursor: default;
		transition:
			background var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease);
	}
	.wc:hover,
	.wc.aktivni {
		background: var(--surface-hover);
		color: var(--text);
	}
	.wc.close {
		color: var(--danger);
	}
	.wc.close:hover {
		background: color-mix(in srgb, var(--danger) 18%, transparent);
		color: var(--danger);
	}
</style>
