<script lang="ts">
	import { prehraj } from './motion';

	// Přepínač zapnuto / vypnuto (role switch). Jen vzhled a ovládání
	// — co přepnutí udělá, řeší volající. „Omezit pohyb" ve Windows
	// vypne posun jezdce i pulz (pravidlo v app.css).

	interface Props {
		zapnuto: boolean;
		zakazano?: boolean;
		/** Čeká na odpověď backendu — jezdec už stojí, kam uživatel chtěl. */
		ceka?: boolean;
		/** Každá nová hodnota přepínačem jednou pulzne („nejdřív zapni ovladač"). */
		pulz?: number;
		popis: string;
		title?: string;
		onprepni: () => void;
	}

	let { zapnuto, zakazano = false, ceka = false, pulz = 0, popis, title, onprepni }: Props = $props();
</script>

<button
	class="sw"
	class:on={zapnuto}
	class:ceka
	role="switch"
	aria-checked={zapnuto}
	aria-label={popis}
	{title}
	disabled={zakazano}
	onclick={onprepni}
	use:prehraj={{ trida: 'kp-pulz-jednou', id: pulz }}
>
	<span class="jezdec"></span>
</button>

<style>
	.sw {
		position: relative;
		flex: none;
		width: 40px;
		height: 22px;
		padding: 0;
		border: 1px solid var(--border-strong);
		border-radius: 999px;
		background: var(--surface);
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease),
			opacity var(--t-fast) var(--ease);
	}
	.sw:hover:not(:disabled) {
		border-color: color-mix(in srgb, var(--accent) 35%, transparent);
	}
	/* Zapnuto: zelená jen jako nádech — plná zelená patří tečce stavu
	   (a ta svítí až u opravdu připojeného ovladače). */
	.sw.on {
		border-color: color-mix(in srgb, var(--ok) 55%, transparent);
		background: color-mix(in srgb, var(--ok) 22%, transparent);
	}
	.sw:disabled {
		opacity: 0.4;
		cursor: default;
	}
	.sw.ceka {
		cursor: progress;
	}
	.jezdec {
		position: absolute;
		top: 2px;
		left: 2px;
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--text-dim);
		transition:
			transform 160ms var(--ease),
			background var(--t-fast) var(--ease);
	}
	.sw.on .jezdec {
		transform: translateX(18px);
		background: var(--accent);
	}
</style>
