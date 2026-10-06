<script lang="ts">
	import Plus from 'lucide-svelte/icons/plus';

	// „+ Ovladač": další karta pro dalšího hráče. Nic nepřipojí ani
	// neuloží — jen ukáže kartu (zapne ji až její přepínač, princip 11).
	// Prázdný přidaný ovladač po restartu zmizí (OQ 52); s klávesami
	// zůstane, protože karty se odvozují z mapování.

	interface Props {
		/** Číslo ovladače, který přibude (0–3) — do bubliny a pro test okna. */
		pad: number;
		onpridej: () => void;
	}

	let { pad, onpridej }: Props = $props();
</script>

<button class="pridat" data-pridat={pad + 1} title="Ovladač {pad + 1} pro dalšího hráče" onclick={onpridej}>
	<Plus size={13} strokeWidth={2} />
	Ovladač
</button>

<style>
	/* Tichá čárkovaná plocha (jako prázdná čepička) — nabídka, ne
	   výzva. Čárky po stranách textu dělá rámeček, ne znaky. */
	.pridat {
		flex-shrink: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.35rem;
		width: 100%;
		height: 34px;
		padding: 0;
		border: 1px dashed var(--border-strong);
		border-radius: var(--radius-lg);
		background: none;
		color: var(--text-faint);
		font-size: var(--fs-sm);
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease);
	}
	.pridat:hover {
		background: var(--surface);
		border-color: color-mix(in srgb, var(--accent) 28%, transparent);
		color: var(--text-dim);
	}
</style>
