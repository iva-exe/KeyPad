<script lang="ts">
	import Minus from 'lucide-svelte/icons/minus';
	import X from 'lucide-svelte/icons/x';
	import { pad } from './pad.svelte';
	import { hlavniOkno } from './tauri';

	// Tečka a popisek stavu virtuálního padu. Zelená jen tehdy, když
	// ovladač opravdu existuje a přijal neutrál (WinSent: zelená nikdy
	// bez skutečnosti); do té doby neutrální, u poruchy červená.
	const popisek = $derived(
		{
			connecting: 'připojuji…',
			connected: 'gamepad připojen',
			bus_missing: 'ViGEmBus chybí',
			bus_not_running: 'ovladač neběží',
			error: 'chyba padu',
			suspended: 'odpojeno'
		}[pad.state]
	);

	const tooltip = $derived.by(() => {
		switch (pad.state) {
			case 'connected':
				return pad.player
					? `Virtuální ovladač Xbox 360 je připojený — hráč ${pad.player} (XInput)`
					: 'Virtuální ovladač Xbox 360 je připojený';
			case 'connecting':
				return pad.detail || 'Připojuji virtuální ovladač Xbox 360…';
			case 'bus_missing':
				return 'Chybí ovladač ViGEmBus — bez něj virtuální gamepad nevznikne';
			case 'bus_not_running':
				return pad.detail || 'Ovladač ViGEmBus je nainstalovaný, ale neběží';
			case 'error':
				return pad.detail ? `Virtuální ovladač nefunguje: ${pad.detail}` : 'Virtuální ovladač nefunguje';
			case 'suspended':
				return 'Virtuální ovladač je odpojený kvůli spánku počítače';
		}
	});
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

	<div class="pad" data-stav={pad.state} title={tooltip}>
		<span class="dot"></span>
		<span class="pad-label">{popisek}</span>
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
		<!-- Zavření okno jen schová (backend, on_window_event): pad
		     zůstává připojený. Ukončit jde z ikony v oznamovací oblasti. -->
		<button
			class="wc close"
			title="Zavřít — KeyPad poběží dál v oznamovací oblasti (ukončit jde z její nabídky)"
			aria-label="Zavřít do oznamovací oblasti"
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
		transition:
			background var(--t-fast) var(--ease),
			box-shadow var(--t-fast) var(--ease);
	}
	/* Připojování: jemné pulzování — je vidět, že se něco děje, ale
	   nekřičí. „Omezit pohyb" ve Windows ho vypne (app.css). */
	.pad[data-stav='connecting'] .dot {
		background: var(--text-dim);
		animation: pulz 1.6s ease-in-out infinite;
	}
	.pad[data-stav='connected'] .dot {
		background: var(--ok);
		box-shadow: var(--glow-ok);
	}
	.pad[data-stav='bus_missing'] .dot,
	.pad[data-stav='error'] .dot {
		background: var(--danger);
		box-shadow: var(--glow-danger);
	}
	/* Nainstalovaný, ale neběžící ViGEmBus: úkol pro uživatele, ne porucha. */
	.pad[data-stav='bus_not_running'] .dot {
		background: var(--warn);
		box-shadow: var(--glow-warn);
	}
	@keyframes pulz {
		0%,
		100% {
			opacity: 0.35;
		}
		50% {
			opacity: 1;
		}
	}
	.pad-label {
		font-family: var(--font-mono);
		font-size: var(--fs-2xs);
		letter-spacing: 0.04em;
		color: var(--text-dim);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.pad[data-stav='bus_missing'] .pad-label,
	.pad[data-stav='error'] .pad-label {
		color: var(--danger);
	}
	.pad[data-stav='bus_not_running'] .pad-label {
		color: var(--warn);
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
