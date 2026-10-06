// Stráž kláves psaných do vlastního okna (spec 1.8).
//
// Bez zapnutého ovladače jdou stisky do WebView — a hráč, který má okno
// KeyPadu v popředí, mačká herní klávesy. Mezerník (vstup A) by „klikl"
// na přepínač s fokusem a připojil ovladač (princip 11), šipky by
// posouvaly panel. Proto okno klávesám nedovolí nic udělat, kromě
// navigace Tabem; Enter a mezerník projdou jen na prvku, kam se
// uživatel právě dostal Tabem. Zkratky prohlížeče (F5, Ctrl+R, Ctrl+P)
// vypíná backend ve WebView2 — sem nedojdou.
//
// Jak se uživatel k fokusu dostal, si stráž vede sama. `:focus-visible`
// to v keydown neřekne: Chromium (a s ním WebView2) ho prvku s fokusem
// od myši přizná už při prvním stisku klávesy, ještě před rozesláním
// keydown. Fokus od myši bez události click (stisk na přepínači a puštění
// jinde, pravý klik na čepičku) by pak první herní mezerník „klikl" —
// ověřeno důvěryhodným vstupem přes CDP v Edge 154 (revize B5).

/** Stisk, jak ho stráž potřebuje — bez DOM, aby šla zkoušet v `bun test`. */
export interface Stisk {
	key: string;
	ctrlKey: boolean;
	altKey: boolean;
	metaKey: boolean;
}

function navigaceTabem(e: Stisk): boolean {
	return e.key === 'Tab' && !e.ctrlKey && !e.altKey && !e.metaKey;
}

/** Enter a mezerník aktivují prvek — smí jen hned po navigaci Tabem. */
function aktivace(e: Stisk): boolean {
	return e.key === 'Enter' || e.key === ' ';
}

/**
 * Shift (Shift+Tab) a samotné modifikátory na navigaci nic nemění.
 * Ostatní klávesy jsou herní — po nich už Enter ani mezerník nepatří
 * navigaci, i kdyby fokus zůstal tam, kam ho poslal Tab.
 */
function modifikator(e: Stisk): boolean {
	return e.key === 'Shift' || e.key === 'Control' || e.key === 'Alt' || e.key === 'Meta';
}

/**
 * Ctrl+C s označeným textem (chyba, cesta k záloze): kopírování nic
 * nepřepne ani neposune, a chybu má jít zkopírovat.
 */
function kopirovani(e: Stisk): boolean {
	if (!e.ctrlKey || e.altKey || e.metaKey) return false;
	return e.key === 'c' || e.key === 'C' || e.key === 'Insert';
}

/** Rozhodování stráže (bez DOM). Každá metoda vrací `true` = zrušit výchozí akci. */
export interface Straz {
	/**
	 * keydown. `fokusViditelny` = cíl odpovídá `:focus-visible` (jen
	 * pojistka navíc, sám nestačí); `vyber` = v okně je označený text.
	 */
	dolu(e: Stisk, fokusViditelny: boolean, vyber: boolean): boolean;
	/** keyup — mezerník tlačítko „klikne" až při puštění. */
	nahoru(e: Stisk, fokusViditelny: boolean): boolean;
	/** Myš, dotyk nebo pero (pointerdown) — fokus od teď není z klávesnice. */
	ukazatel(): void;
	/** Okno ztratilo popředí — po návratu do něj můžou klávesy patřit hře. */
	ztrataPopredi(): void;
}

export function vytvorStraz(): Straz {
	// Fokus poslal Tab a od té doby nebyla myš, herní klávesa ani ztráta
	// popředí. Začíná `false`: po startu nemá fokus nic, co by šlo stisknout.
	let zTabu = false;
	const smiAktivovat = (fokusViditelny: boolean) => zTabu && fokusViditelny;
	return {
		dolu(e, fokusViditelny, vyber) {
			if (navigaceTabem(e)) {
				zTabu = true;
				return false;
			}
			if (kopirovani(e) && vyber) return false;
			if (aktivace(e)) return !smiAktivovat(fokusViditelny);
			if (!modifikator(e)) zTabu = false;
			return true;
		},
		nahoru(e, fokusViditelny) {
			return aktivace(e) && !smiAktivovat(fokusViditelny);
		},
		ukazatel() {
			zTabu = false;
		},
		ztrataPopredi() {
			zTabu = false;
		}
	};
}

function fokusZKlavesnice(cil: EventTarget | null): boolean {
	return cil instanceof Element && cil.matches(':focus-visible');
}

function oznacenyText(): boolean {
	const v = document.getSelection();
	return !!v && !v.isCollapsed;
}

/**
 * Tlačítko po kliknutí myší pustí fokus — pojistka navíc ke stráži:
 * fokus od myši nemá na tlačítku co dělat. `detail` je u kliku myší
 * počet kliknutí, u aktivace z klávesnice 0 — tam fokus zůstává, aby
 * navigace Tabem pokračovala, kde byla.
 */
function klik(e: MouseEvent): void {
	if (e.detail === 0) return;
	const tlacitko = e.target instanceof Element ? e.target.closest('button') : null;
	if (tlacitko && tlacitko === document.activeElement) tlacitko.blur();
}

/** Zapne stráž pro celé okno; vrací úklid. */
export function hlidejKlavesnici(): () => void {
	const straz = vytvorStraz();
	const dolu = (e: KeyboardEvent) => {
		// Výběr textu se zjišťuje jen u Ctrl — jinak ho stráž nepotřebuje.
		if (straz.dolu(e, fokusZKlavesnice(e.target), e.ctrlKey && oznacenyText())) e.preventDefault();
	};
	const nahoru = (e: KeyboardEvent) => {
		if (straz.nahoru(e, fokusZKlavesnice(e.target))) e.preventDefault();
	};
	const ukazatel = () => straz.ukazatel();
	const ztrata = () => straz.ztrataPopredi();
	// Zachytávací fáze: stráž musí rozhodnout dřív, než stisk dojde
	// k tlačítku nebo k obsluze komponenty — a stisk myši zaznamenat
	// dřív, než tlačítko dostane fokus.
	window.addEventListener('keydown', dolu, true);
	window.addEventListener('keyup', nahoru, true);
	window.addEventListener('pointerdown', ukazatel, true);
	window.addEventListener('blur', ztrata);
	document.addEventListener('click', klik, true);
	return () => {
		window.removeEventListener('keydown', dolu, true);
		window.removeEventListener('keyup', nahoru, true);
		window.removeEventListener('pointerdown', ukazatel, true);
		window.removeEventListener('blur', ztrata);
		document.removeEventListener('click', klik, true);
	};
}
