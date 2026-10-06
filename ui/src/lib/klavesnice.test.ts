// Stráž kláves psaných do okna (spec 1.8) — rozhodování bez DOM.
//
// `fokusViditelny` tu modeluje Chromium (WebView2): prvku s fokusem od
// myši přizná `:focus-visible` už při prvním stisku klávesy, ještě před
// rozesláním keydown. Proto ho testy podávají `true` i po myši — přesně
// tak prošel herní mezerník přes stráž, která věřila jen `:focus-visible`
// (revize B5: stisk na přepínači a puštění jinde, pravý klik na čepičku).
// Důvěryhodný vstup v celém okně ověřuje test na skryté ploše (B6).
import { describe, expect, test } from 'bun:test';
import { type Stisk, vytvorStraz } from './klavesnice';

function stisk(key: string, ctrlKey = false): Stisk {
	return { key, ctrlKey, altKey: false, metaKey: false };
}

const TAB = stisk('Tab');
const MEZERNIK = stisk(' ');
const ENTER = stisk('Enter');

describe('stráž kláves', () => {
	test('bez navigace Tabem nic neprojde', () => {
		const s = vytvorStraz();
		for (const k of [MEZERNIK, ENTER, stisk('w'), stisk('ArrowDown'), stisk('F5'), stisk('Escape')]) {
			expect(s.dolu(k, false, false)).toBe(true);
		}
		expect(s.nahoru(MEZERNIK, false)).toBe(true);
		expect(s.nahoru(stisk('w'), false)).toBe(false);
	});

	test('fokus od myši: mezerník ani Enter neaktivují, i když Chromium hlásí :focus-visible', () => {
		const s = vytvorStraz();
		// Stisk na přepínači, puštění jinde (žádný click) — fokus zůstal.
		s.ukazatel();
		expect(s.dolu(MEZERNIK, true, false)).toBe(true);
		expect(s.nahoru(MEZERNIK, true)).toBe(true);
		expect(s.dolu(ENTER, true, false)).toBe(true);
	});

	test('Tab a pak mezerník nebo Enter projdou (navigace klávesnicí)', () => {
		const s = vytvorStraz();
		expect(s.dolu(TAB, false, false)).toBe(false);
		expect(s.dolu(MEZERNIK, true, false)).toBe(false);
		expect(s.nahoru(MEZERNIK, true)).toBe(false);
		expect(s.dolu(ENTER, true, false)).toBe(false);
	});

	test('Shift+Tab je navigace, samotný Shift ji nepřeruší', () => {
		const s = vytvorStraz();
		expect(s.dolu(stisk('Shift'), false, false)).toBe(true);
		expect(s.dolu(TAB, false, false)).toBe(false);
		expect(s.dolu(MEZERNIK, true, false)).toBe(false);
	});

	test('myš po Tabu navigaci ukončí', () => {
		const s = vytvorStraz();
		s.dolu(TAB, false, false);
		s.ukazatel();
		expect(s.dolu(MEZERNIK, true, false)).toBe(true);
		expect(s.nahoru(MEZERNIK, true)).toBe(true);
	});

	test('ztráta popředí po Tabu navigaci ukončí (po návratu může hrát hra)', () => {
		const s = vytvorStraz();
		s.dolu(TAB, false, false);
		s.ztrataPopredi();
		expect(s.dolu(MEZERNIK, true, false)).toBe(true);
	});

	test('herní klávesa po Tabu navigaci ukončí', () => {
		const s = vytvorStraz();
		s.dolu(TAB, false, false);
		expect(s.dolu(stisk('w'), true, false)).toBe(true);
		expect(s.dolu(MEZERNIK, true, false)).toBe(true);
		expect(s.nahoru(MEZERNIK, true)).toBe(true);
	});

	test('mezerník propustí stráž jen s :focus-visible (pojistka navíc)', () => {
		const s = vytvorStraz();
		s.dolu(TAB, false, false);
		expect(s.dolu(MEZERNIK, false, false)).toBe(true);
		expect(s.nahoru(MEZERNIK, false)).toBe(true);
	});

	test('Ctrl+C projde jen s označeným textem', () => {
		const s = vytvorStraz();
		expect(s.dolu(stisk('c', true), false, true)).toBe(false);
		expect(s.dolu(stisk('Insert', true), false, true)).toBe(false);
		expect(s.dolu(stisk('c', true), false, false)).toBe(true);
	});

	test('Ctrl+Tab ani Alt+Tab nejsou navigace', () => {
		const s = vytvorStraz();
		expect(s.dolu({ key: 'Tab', ctrlKey: true, altKey: false, metaKey: false }, false, false)).toBe(true);
		expect(s.dolu({ key: 'Tab', ctrlKey: false, altKey: true, metaKey: false }, false, false)).toBe(true);
		expect(s.dolu(MEZERNIK, true, false)).toBe(true);
	});
});
