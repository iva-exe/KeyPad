<script lang="ts">
	import Download from 'lucide-svelte/icons/download';
	import ExternalLink from 'lucide-svelte/icons/external-link';
	import RotateCw from 'lucide-svelte/icons/rotate-cw';
	import TriangleAlert from 'lucide-svelte/icons/triangle-alert';
	import { slide } from 'svelte/transition';
	import { aplikace } from './aplikace.svelte';
	import { klavesy } from './klavesy.svelte';
	import { trvani, zpomaleni } from './motion';
	import { instalatorBezi, instalujOvladac, otevri, pady, sbernice, zapnuto, zkusZnovu } from './pady.svelte';
	import { bublinaChyb } from './vstupy';

	// Pruh nad kartami — jen když je problém, který se netýká jednoho
	// ovladače: ViGEmBus (chybí / neběží / je starší / běží instalátor)
	// a uložení kláves. Stav sběrnice hlásí vlákno ovladače 1, které běží
	// vždy. Jedna krátká věta a tlačítko, podrobnosti v bublině.

	const p0 = $derived(pady[0]!);
	const instalator = $derived(instalatorBezi());
	const chybi = $derived(p0.state === 'bus_missing');
	const nebezi = $derived(p0.state === 'bus_not_running');
	const stary = $derived(p0.stary && !chybi);

	// Instalaci i aktualizaci umí jen KeyPadSetup z instalační složky;
	// bez něj zbývá ruční stažení. Zapnuté ovladače backend před
	// spuštěním instalátoru sám vypne (neutrál → odpojit).
	const instalace = $derived(chybi && aplikace.setup);
	const aktualizace = $derived(stary && aplikace.setup);
	// Rada k „zbytku ovladače" má adresu vydání — tlačítko otevře pevnou
	// stránku (backend), žádnou adresu z textu.
	const rucne = $derived((nebezi && /https:\/\//.test(p0.detail)) || ((chybi || stary) && !aplikace.setup));

	const bublinaInstalace = $derived(
		instalace
			? 'Oficiální instalátor ViGEmBus — Windows se zeptají na oprávnění správce'
			: pady.some((_, i) => zapnuto(i))
				? 'Novější ViGEmBus — ovladače se nejdřív vypnou; Windows se zeptají na oprávnění správce'
				: 'Novější ViGEmBus — Windows se zeptají na oprávnění správce'
	);

	type Druh = 'chyba' | 'pozor' | 'tichy';

	const sbernicovy = $derived.by((): { druh: Druh; text: string; titulek: string } | null => {
		if (instalator) {
			return {
				druh: 'tichy',
				text: 'Instalátor ViGEmBus běží…',
				titulek: 'Ovladače půjdou zapnout po jeho konci'
			};
		}
		if (chybi) {
			return {
				druh: 'chyba',
				text: 'Chybí ViGEmBus',
				titulek: aplikace.setup
					? 'Bez ovladače ViGEmBus virtuální gamepad nevznikne. Instalaci potvrdíš ve výzvě Windows.'
					: 'Bez ovladače ViGEmBus virtuální gamepad nevznikne. Stáhni ho ze stránky vydání.'
			};
		}
		if (nebezi) return { druh: 'pozor', text: 'ViGEmBus neběží', titulek: p0.detail };
		if (stary) return { druh: 'tichy', text: 'Novější ViGEmBus', titulek: bublinaInstalace };
		return null;
	});

	// Uložení kláves (config.json): obnovené výchozí klávesy, nebo soubor,
	// který KeyPad nesmí či nemůže přepsat. Jantarově — upozornění, ne porucha.
	const konfiguracni = $derived.by((): { text: string; titulek: string } | null => {
		switch (klavesy.konfigurace) {
			case 'obnovena':
				// Bez cesty nic neslibovat: nevalidní soubor, který nešel
				// odložit, backend hlásí jako `necitelna` (princip 8).
				return {
					text: 'Klávesy nešly načíst — výchozí',
					titulek: [
						klavesy.zaloha ? `Původní soubor je odložený v ${klavesy.zaloha}.` : '',
						bublinaChyb(klavesy.chyby)
					]
						.filter(Boolean)
						.join('\n')
				};
			case 'novejsi':
				return {
					text: 'Klávesy se neukládají',
					titulek: 'Soubor s klávesami je z novější verze KeyPadu — tahle ho nepřepíše. Platí výchozí klávesy.'
				};
			case 'necitelna':
				return {
					text: 'Klávesy se neukládají',
					// Nečitelný soubor i nevalidní, který nešel odložit
					// do zálohy: obojí „nejde načíst" a přepsat se nesmí.
					titulek: `Soubor s klávesami nejde načíst, proto se nepřepisuje. Platí výchozí klávesy.\n${bublinaChyb(klavesy.chyby)}`
				};
			case 'neulozena':
				return {
					text: 'Klávesy se neukládají',
					titulek: 'Zápis se nepovedl — zkusí se znovu při další změně. Podrobnosti jsou v logu.'
				};
			default:
				return null;
		}
	});

	let chybaOdkazu = $state('');
	async function stahni(): Promise<void> {
		chybaOdkazu = await otevri('vigembus_releases');
	}
	const chyba = $derived(sbernice.chyba || chybaOdkazu);

	// Nejvážnější řádek určí barvu celého pruhu.
	const druh = $derived<Druh>(
		sbernicovy?.druh === 'chyba' ? 'chyba' : sbernicovy?.druh === 'pozor' || konfiguracni ? 'pozor' : 'tichy'
	);
	const videt = $derived(sbernicovy !== null || konfiguracni !== null || chyba !== '');
</script>

{#if videt}
	<div class="obal" transition:slide={{ duration: trvani(140), easing: zpomaleni }}>
		<div class="pruh" data-druh={druh} data-sbernice={p0.state} data-konfigurace={klavesy.konfigurace}>
			{#if sbernicovy}
				<div class="radek" data-druh={sbernicovy.druh}>
					<span class="ikona" aria-hidden="true"><TriangleAlert size={14} strokeWidth={1.9} /></span>
					<span class="text" title={sbernicovy.titulek}>{sbernicovy.text}</span>
					<span class="akce">
						{#if instalace || aktualizace}
							<button
								class="btn"
								class:primary={instalace}
								disabled={instalator}
								title={bublinaInstalace}
								onclick={() => void instalujOvladac()}
							>
								<Download size={13} strokeWidth={1.9} />
								{instalace ? 'Nainstalovat' : 'Aktualizovat'}
							</button>
						{/if}
						{#if rucne && !instalator}
							<button class="btn" title="Stránka vydání ViGEmBus v prohlížeči" onclick={() => void stahni()}>
								<ExternalLink size={12} strokeWidth={1.9} />
								Stáhnout
							</button>
						{/if}
						{#if (chybi || nebezi) && !instalator}
							<!-- Jen ověří sběrnici, nic nepřipojí (princip 11). Se
							     slovy jen jako jediné tlačítko — vedle jiného by
							     v úzkém okně utnulo větu o ViGEmBus. -->
							{#if !(instalace || aktualizace || rucne)}
								<button class="btn" onclick={() => void zkusZnovu()}>
									<RotateCw size={12} strokeWidth={1.9} />
									Zkusit znovu
								</button>
							{:else}
								<button
									class="btn ikonove"
									title="Zkusit znovu"
									aria-label="Zkusit znovu"
									onclick={() => void zkusZnovu()}
								>
									<RotateCw size={13} strokeWidth={1.9} />
								</button>
							{/if}
						{/if}
					</span>
				</div>
			{/if}

			{#if konfiguracni}
				<div class="radek" data-druh="pozor">
					<span class="ikona" aria-hidden="true"><TriangleAlert size={14} strokeWidth={1.9} /></span>
					<span class="text" title={konfiguracni.titulek}>{konfiguracni.text}</span>
				</div>
			{/if}

			{#if chyba}
				<p class="err selectable">{chyba}</p>
			{/if}
		</div>
	</div>
{/if}

<style>
	/* `slide` animuje výšku obalu — rámeček s pozadím je až uvnitř
	   (stejně jako banner aktualizace). */
	.obal {
		flex-shrink: 0;
	}
	.pruh {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		padding: 0.45rem 0.55rem 0.45rem 0.7rem;
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-lg);
		background: var(--surface);
	}
	/* Barva jen podle významu: chybějící ViGEmBus červeně, neběžící
	   a klávesy, které se neukládají, jantarově (úkol, ne porucha). */
	.pruh[data-druh='chyba'] {
		border-color: color-mix(in srgb, var(--danger) 40%, transparent);
		background: color-mix(in srgb, var(--danger) 7%, var(--surface));
	}
	.pruh[data-druh='pozor'] {
		border-color: color-mix(in srgb, var(--warn) 40%, transparent);
		background: color-mix(in srgb, var(--warn) 7%, var(--surface));
	}

	/* Věta se nezkracuje: když se s tlačítky nevejde, tlačítka spadnou
	   na další řádek (doprava). */
	.radek {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.35rem 0.5rem;
		min-height: 26px;
	}
	.radek + .radek {
		padding-top: 0.35rem;
		border-top: 1px dotted var(--border-strong);
	}
	.ikona {
		flex: none;
		display: grid;
		place-items: center;
		color: var(--text-dim);
	}
	.radek[data-druh='chyba'] .ikona {
		color: var(--danger);
	}
	.radek[data-druh='pozor'] .ikona {
		color: var(--warn);
	}
	.text {
		flex: 1 1 auto;
		min-width: 0;
		font-size: var(--fs-sm);
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.radek[data-druh='tichy'] .text {
		color: var(--text-dim);
	}

	.akce {
		flex: none;
		display: flex;
		gap: 0.35rem;
		margin-left: auto;
	}
	.btn {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		height: 26px;
		padding: 0 9px;
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-sm);
		background: none;
		color: var(--text-dim);
		font-size: var(--fs-xs);
		white-space: nowrap;
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease),
			opacity var(--t-fast) var(--ease);
	}
	.btn:hover:not(:disabled) {
		background: var(--surface-hover);
		border-color: color-mix(in srgb, var(--accent) 30%, transparent);
		color: var(--text);
	}
	.btn:disabled {
		opacity: 0.55;
		cursor: default;
	}
	.btn.ikonove {
		width: 28px;
		padding: 0;
		justify-content: center;
	}
	/* Hlavní akce: bílá (akcent), žádný gradient ani lesk. */
	.btn.primary {
		border-color: transparent;
		background: var(--accent);
		color: var(--na-barve);
		font-weight: 600;
	}
	.btn.primary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent) 88%, transparent);
		color: var(--na-barve);
	}

	.err {
		margin: 0;
		font-size: var(--fs-xs);
		color: var(--danger);
		overflow-wrap: anywhere;
	}
	/* Chybu musí jít označit a zkopírovat. */
	.selectable {
		user-select: text;
		cursor: text;
	}
</style>
