<script lang="ts">
	import ScrollText from 'lucide-svelte/icons/scroll-text';
	import { onMount } from 'svelte';
	import { textChyby, zavolej } from './tauri';

	/** Odpověď příkazu `app_info` (src-tauri/src/main.rs). */
	interface AppInfo {
		version: string;
		log_path: string;
		dev: boolean;
	}

	// Neznámá hodnota = „—", nikdy vymyšlené číslo (WinSent DESIGN.md).
	let verze = $state('—');
	let cestaLogu = $state('');
	let chybaLogu = $state('');
	let casovac: ReturnType<typeof setTimeout> | undefined;

	onMount(() => {
		zavolej<AppInfo>('app_info')
			.then((i) => {
				verze = i.version;
				cestaLogu = i.log_path;
			})
			.catch(() => {
				// V prohlížeči backend není — patička zůstane s „—".
			});
		return () => clearTimeout(casovac);
	});

	async function otevriLog() {
		try {
			await zavolej('open_log_dir');
			chybaLogu = '';
		} catch (e) {
			// Chybu ukázat, ale jen na chvíli: je to odpověď na klik,
			// ne trvalý stav aplikace.
			chybaLogu = textChyby(e);
			clearTimeout(casovac);
			casovac = setTimeout(() => (chybaLogu = ''), 5000);
		}
	}
</script>

<footer class="foot">
	<span class="ver">
		<span class="label-tech">ver</span>
		<span class="value-mono">{verze}</span>
	</span>

	{#if chybaLogu}
		<span class="err" title={chybaLogu}>{chybaLogu}</span>
	{/if}

	<button
		class="log"
		title={cestaLogu ? `Otevřít složku s logem — ${cestaLogu}` : 'Otevřít složku s logem'}
		onclick={() => void otevriLog()}
	>
		<ScrollText size={13} strokeWidth={1.75} />
		<span>log</span>
	</button>
</footer>

<style>
	.foot {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		height: 32px;
		padding: 0 10px 0 16px;
		flex-shrink: 0;
	}
	.ver {
		display: flex;
		align-items: baseline;
		gap: 0.45rem;
		min-width: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.ver .value-mono {
		font-size: var(--fs-2xs);
		color: var(--text-dim);
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
