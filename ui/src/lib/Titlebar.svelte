<script lang="ts">
	import Minus from 'lucide-svelte/icons/minus';
	import X from 'lucide-svelte/icons/x';
	import { hlavniOkno } from './tauri';

	// Stav padu zatím NEEXISTUJE — virtuální ovladač (ViGEm) přijde ve
	// Fázi 2. Tečka proto svítí neutrálně a popisek říká „—", ne „OK":
	// zelená bez skutečného ovladače by lhala (WinSent: zelená nikdy
	// bez vynucení). Až bude pad, přijde sem stav z backendu (událost
	// Tauri) a tečka dostane barvu podle významu.
	const padPopis = 'Virtuální gamepad přijde v další fázi — zatím se nic nepřevádí';
</script>

<!-- „deep": táhne se za celý pruh včetně loga a stavu; tlačítka tažení
     blokují sama (Tauri je pozná jako klikatelné prvky). Dvojklik okno
     nemaximalizuje — v tauri.conf.json je maximizable: false, protože
     nástroj přes celou obrazovku nedává smysl. -->
<header class="titlebar" data-tauri-drag-region="deep">
	<div class="brand">
		<img src="/icon.png" alt="" width="18" height="18" draggable="false" />
		<span class="wordmark">KeyPad</span>
	</div>

	<div class="pad" title={padPopis}>
		<span class="dot"></span>
		<span class="pad-label">gamepad —</span>
	</div>

	<div class="win-controls">
		<button
			class="wc"
			title="Minimalizovat"
			aria-label="Minimalizovat"
			onclick={() => void hlavniOkno()?.minimize()}
		>
			<Minus size={17} strokeWidth={1.75} />
		</button>
		<button
			class="wc close"
			title="Zavřít"
			aria-label="Zavřít"
			onclick={() => void hlavniOkno()?.close()}
		>
			<X size={18} strokeWidth={1.75} />
		</button>
	</div>
</header>

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
	.pad {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		min-width: 0;
	}
	/* Neutrální tečka BEZ glow — šedé prvky nesvítí (glow patří jen
	   významovým barvám). */
	.dot {
		flex: none;
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--text-faint);
	}
	.pad-label {
		font-family: var(--font-mono);
		font-size: var(--fs-2xs);
		letter-spacing: 0.04em;
		color: var(--text-dim);
		white-space: nowrap;
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
	.wc:hover {
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
