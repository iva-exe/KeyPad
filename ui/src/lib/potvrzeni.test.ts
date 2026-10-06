// Potvrzovací dialog ↺ a 🗑 (Fáze 7, Z3) — texty a klávesy jako čisté
// funkce (bun test). Fokus, Esc přes CDP a klik mimo zkouší
// tools\okno-test.ps1 naostro.
import { describe, expect, test } from 'bun:test';
import { klavesaDialogu, textyPotvrzeni, type StiskDialogu } from './potvrzeni';

const s = (key: string, mod: Partial<StiskDialogu> = {}): StiskDialogu => ({
	key,
	shiftKey: false,
	ctrlKey: false,
	altKey: false,
	metaKey: false,
	...mod
});

describe('texty', () => {
	test('↺ výchozí klávesy ovladače 1', () => {
		expect(textyPotvrzeni('vychozi', 0, true)).toEqual({
			nadpis: 'Vrátit výchozí klávesy ovladače 1?',
			popis: 'Klávesy jiných ovladačů zůstanou.',
			akce: 'Vrátit',
			nebezpecne: false
		});
	});

	test('🗑 odebrat ovladač — „klávesy se smažou" jen má-li je', () => {
		expect(textyPotvrzeni('odebrat', 2, true)).toEqual({
			nadpis: 'Odebrat ovladač 3?',
			popis: 'Jeho klávesy se smažou.',
			akce: 'Odebrat',
			nebezpecne: true
		});
		expect(textyPotvrzeni('odebrat', 1, false).popis).toBe('');
	});
});

describe('klávesy dialogu', () => {
	test('Esc = Zrušit (i s fokusem kdekoli), Esc s modifikátorem ne', () => {
		expect(klavesaDialogu(s('Escape'), 0, 2)).toEqual({ akce: 'zrusit' });
		expect(klavesaDialogu(s('Escape'), -1, 2)).toEqual({ akce: 'zrusit' });
		expect(klavesaDialogu(s('Escape', { shiftKey: true }), 0, 2)).toEqual({ akce: 'zrusit' });
		expect(klavesaDialogu(s('Escape', { ctrlKey: true }), 0, 2)).toBeNull();
	});

	test('Tab a Shift+Tab chodí dokola jen po tlačítkách dialogu', () => {
		expect(klavesaDialogu(s('Tab'), 0, 2)).toEqual({ akce: 'fokus', index: 1 });
		expect(klavesaDialogu(s('Tab'), 1, 2)).toEqual({ akce: 'fokus', index: 0 });
		expect(klavesaDialogu(s('Tab', { shiftKey: true }), 0, 2)).toEqual({ akce: 'fokus', index: 1 });
		expect(klavesaDialogu(s('Tab', { shiftKey: true }), 1, 2)).toEqual({ akce: 'fokus', index: 0 });
		// Fokus mimo dialog (nemělo by nastat): Tab ho vrátí dovnitř.
		expect(klavesaDialogu(s('Tab'), -1, 2)).toEqual({ akce: 'fokus', index: 0 });
		expect(klavesaDialogu(s('Tab', { shiftKey: true }), -1, 2)).toEqual({ akce: 'fokus', index: 1 });
		// Alt+Tab patří Windows.
		expect(klavesaDialogu(s('Tab', { altKey: true }), 0, 2)).toBeNull();
	});

	test('Enter, mezerník a ostatní klávesy dialog neřeší (stráž kláves okna)', () => {
		for (const k of ['Enter', ' ', 'a', 'ArrowDown', 'F5']) expect(klavesaDialogu(s(k), 0, 2)).toBeNull();
	});
});
