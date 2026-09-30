<script lang="ts">
	import ExternalLink from 'lucide-svelte/icons/external-link';
	import ScrollText from 'lucide-svelte/icons/scroll-text';
	import { onMount } from 'svelte';
	import { fly } from 'svelte/transition';
	import { aplikace } from './aplikace.svelte';
	import { trvani, zpomaleni } from './motion';
	import { otevri } from './pad.svelte';
	import { textChyby, zavolej } from './tauri';

	// „O aplikaci": verze, credit ViGEmBus (bez něj by KeyPad nebyl)
	// a log. Nic víc — podrobnosti jsou v logu.

	interface Props {
		/** Zavřít (Esc, klik mimo). */
		onzavri: () => void;
		/** Tlačítko, které panel otevřelo — klik na něj panel přepíná sám. */
		kotva: HTMLElement | undefined;
	}

	let { onzavri, kotva }: Props = $props();

	let panel: HTMLElement | undefined = $state();
	let chyba = $state('');

	onMount(() => {
		// Fokus do panelu: Esc pak funguje hned a čtečka ho přečte.
		panel?.focus();
	});

	function mimo(e: PointerEvent) {
		const cil = e.target as Node | null;
		if (!cil || panel?.contains(cil) || kotva?.contains(cil)) return;
		onzavri();
	}

	function klavesa(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			e.preventDefault();
			onzavri();
			kotva?.focus();
		}
	}

	async function odkaz() {
		chyba = await otevri('vigembus');
	}

	async function log() {
		try {
			await zavolej('open_log_dir');
			chyba = '';
		} catch (e) {
			chyba = textChyby(e);
		}
	}
</script>

<svelte:window onpointerdown={mimo} onkeydown={klavesa} />

<div
	class="about"
	role="dialog"
	aria-label="O aplikaci"
	tabindex="-1"
	bind:this={panel}
	transition:fly={{ y: -6, duration: trvani(140), easing: zpomaleni }}
>
	<div class="radek">
		<span class="label-tech">keypad</span>
		<span class="value-mono ver" title={aplikace.verze}>{aplikace.verze}</span>
	</div>

	<div class="radek credit">
		<span class="label-tech">ovladač</span>
		<!-- Text i adresa z backendu (updater::vigembus); klik otevírá
		     backend podle jména, adresu z okna nebere. -->
		<button
			class="odkaz"
			title={aplikace.vigembusAdresa.replace(/^https:\/\//, '') || undefined}
			onclick={() => void odkaz()}
		>
			<span>{aplikace.vigembusCredit || 'ViGEmBus'}</span>
			<ExternalLink size={12} strokeWidth={1.9} />
		</button>
		{#if aplikace.vigembusLicence}
			<span class="lic value-mono">{aplikace.vigembusLicence}</span>
		{/if}
	</div>

	<div class="pata">
		{#if chyba}
			<span class="err" title={chyba}>{chyba}</span>
		{/if}
		<button
			class="log"
			title={aplikace.cestaLogu ? `Otevřít složku s logem — ${aplikace.cestaLogu}` : 'Otevřít složku s logem'}
			onclick={() => void log()}
		>
			<ScrollText size={13} strokeWidth={1.75} />
			<span>log</span>
		</button>
	</div>
</div>

<style>
	.about {
		position: fixed;
		top: 42px;
		right: 12px;
		z-index: 10;
		width: min(300px, calc(100vw - 24px));
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
		padding: 0.8rem 0.9rem 0.6rem;
		/* Neprůhledné: pod panelem je karta a text by prosvítal. */
		background: var(--popover);
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-lg);
		box-shadow: 0 10px 28px rgba(0, 0, 0, 0.45);
		outline: none;
	}

	.radek {
		display: grid;
		grid-template-columns: 4.6rem 1fr;
		align-items: baseline;
		gap: 0.15rem 0.6rem;
	}
	.ver {
		font-size: var(--fs-2xs);
		color: var(--text-dim);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.credit .lic {
		grid-column: 2;
		font-size: var(--fs-3xs);
		color: var(--text-faint);
	}
	/* Inline: dlouhé jméno se zalomí a ikona jde za posledním slovem,
	   ne do rohu. */
	.odkaz {
		display: inline;
		justify-self: start;
		padding: 0;
		border: 0;
		background: none;
		color: var(--text);
		font-size: var(--fs-sm);
		text-align: left;
		cursor: pointer;
		transition: color var(--t-fast) var(--ease);
	}
	.odkaz span {
		text-decoration: underline;
		text-decoration-color: var(--border-strong);
		text-underline-offset: 3px;
		transition: text-decoration-color var(--t-fast) var(--ease);
	}
	.odkaz :global(svg) {
		margin-left: 0.3rem;
		vertical-align: -1px;
		color: var(--text-faint);
	}
	.odkaz:hover span {
		text-decoration-color: var(--text-dim);
	}

	.pata {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding-top: 0.5rem;
		border-top: 1px dotted var(--border-strong);
	}
	.err {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--fs-xs);
		color: var(--danger);
	}
	.log {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 0.35rem;
		padding: 3px 8px;
		border: 1px solid transparent;
		border-radius: var(--radius-sm);
		background: none;
		color: var(--text-faint);
		font-family: var(--font-mono);
		font-size: var(--fs-2xs);
		text-transform: uppercase;
		letter-spacing: 0.08em;
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease);
	}
	.log:hover {
		background: var(--surface-hover);
		border-color: var(--border);
		color: var(--text);
	}
</style>
