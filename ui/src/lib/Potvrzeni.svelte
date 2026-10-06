<script lang="ts">
	import { fade, scale } from 'svelte/transition';
	import { trvani, zpomaleni } from './motion';
	import { klavesaDialogu, textyPotvrzeni } from './potvrzeni';
	import { potvrd, potvrzeni, zavriPotvrzeni } from './dialog.svelte';

	// Potvrzení ↺ a 🗑 jedním klikem (Fáze 7, Z3): modální vrstva přes panel
	// s kartami, ne okno Windows. Fokus začíná na „Zrušit" (bezpečná volba),
	// Tab chodí jen mezi tlačítky dialogu, Esc i klik mimo dialog = Zrušit.
	// Enter a mezerník projdou jen s fokusem z Tabu (stráž kláves okna) —
	// dialog otevřený myší tak Enter nepotvrdí.

	const t = $derived(textyPotvrzeni(potvrzeni.druh ?? 'vychozi', potvrzeni.pad, potvrzeni.maKlavesy));

	let dialog: HTMLElement | undefined = $state();
	let zrusit: HTMLButtonElement | undefined = $state();

	// Fokus na „Zrušit" s každým otevřením, ne jen při vzniku: otevření
	// během zavíracího přechodu (↺ z klávesnice do 120 ms) vrátí tentýž
	// dialog, nový nevznikne.
	$effect(() => {
		void potvrzeni.id;
		zrusit?.focus();
	});

	function tlacitka(): HTMLButtonElement[] {
		return dialog ? [...dialog.querySelectorAll('button')] : [];
	}

	// Během zavíracího přechodu je dialog ještě v DOM, ale už neplatí:
	// Tab by fokus poslal do mizejících tlačítek.
	function klavesa(e: KeyboardEvent): void {
		if (potvrzeni.druh === null) return;
		const b = tlacitka();
		const a = klavesaDialogu(e, b.indexOf(document.activeElement as HTMLButtonElement), b.length);
		if (!a) return;
		e.preventDefault();
		if (a.akce === 'zrusit') zavriPotvrzeni();
		else b[a.index]?.focus();
	}

	/** Klik mimo dialog (i do titulku okna) = Zrušit. */
	function mimo(e: PointerEvent): void {
		if (potvrzeni.druh === null) return;
		const cil = e.target as Node | null;
		if (cil && dialog?.contains(cil)) return;
		zavriPotvrzeni(false);
	}

	/** Schované okno (×, minimalizace) dialog zruší — nic se nezmění. */
	function viditelnost(): void {
		if (document.hidden) zavriPotvrzeni(false);
	}
</script>

<svelte:window onkeydown={klavesa} onpointerdowncapture={mimo} />
<svelte:document onvisibilitychange={viditelnost} />

<!-- Vrstva zmizí stejně plynule jako dialog — jinak by po jeho zmizení
     tmavé pozadí ještě chvíli viselo a pak naráz zhaslo. -->
<div class="vrstva" data-potvrzeni={potvrzeni.druh} transition:fade={{ duration: trvani(120), easing: zpomaleni }}>
	<div
		class="dialog"
		role="alertdialog"
		aria-modal="true"
		aria-labelledby="potvrzeni-nadpis"
		aria-describedby={t.popis ? 'potvrzeni-popis' : undefined}
		bind:this={dialog}
		transition:scale={{ start: 0.96, duration: trvani(120), easing: zpomaleni }}
	>
		<p id="potvrzeni-nadpis" class="nadpis">{t.nadpis}</p>
		{#if t.popis}<p id="potvrzeni-popis" class="popis">{t.popis}</p>{/if}
		<div class="tlacitka">
			<button class="zrusit" data-akce="zrusit" bind:this={zrusit} onclick={() => zavriPotvrzeni()}>Zrušit</button>
			<button class="potvrdit" class:nebezpecne={t.nebezpecne} data-akce="potvrdit" onclick={() => void potvrd()}>
				{t.akce}
			</button>
		</div>
	</div>
</div>

<style>
	/* Pod titulkem: minimalizovat a schovat jde i s otevřeným dialogem
	   (schování ho zruší). */
	.vrstva {
		position: fixed;
		inset: 44px 0 0 0;
		z-index: 20;
		display: grid;
		place-items: center;
		padding: 16px;
		background: rgba(8, 9, 12, 0.55);
	}
	.dialog {
		width: min(300px, 100%);
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		padding: 0.9rem 0.95rem 0.75rem;
		/* Neprůhledné jako panel ⓘ — pod ním jsou karty. */
		background: var(--popover);
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-lg);
		box-shadow: 0 10px 28px rgba(0, 0, 0, 0.45);
	}
	.nadpis {
		margin: 0;
		font-size: var(--fs-lg);
		font-weight: 500;
		color: var(--text);
	}
	.popis {
		margin: 0;
		font-size: var(--fs-sm);
		color: var(--text-dim);
	}
	.tlacitka {
		display: flex;
		justify-content: flex-end;
		gap: 0.45rem;
		margin-top: 0.4rem;
	}
	.tlacitka button {
		min-width: 5.2rem;
		padding: 5px 12px;
		border: 1px solid var(--border-strong);
		border-radius: var(--radius);
		background: var(--surface);
		color: var(--text);
		font-size: var(--fs-sm);
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease);
	}
	.tlacitka button:hover {
		background: var(--surface-hover);
	}
	.potvrdit {
		border-color: color-mix(in srgb, var(--accent) 40%, transparent);
	}
	/* Mazání červeně — barva jen podle významu. */
	.potvrdit.nebezpecne {
		border-color: color-mix(in srgb, var(--danger) 60%, transparent);
		background: color-mix(in srgb, var(--danger) 16%, transparent);
		color: var(--danger);
	}
	.potvrdit.nebezpecne:hover {
		background: color-mix(in srgb, var(--danger) 24%, transparent);
	}
</style>
