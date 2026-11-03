// Formatting shared by the pages.

export interface SummaryLine {
	label: string;
	value: string;
}

export interface ModelStatus {
	loading?: boolean;
	file?: string;
	sha1?: string;
	promoted?: string;
	untrained: number;
	error?: string;
}

/** A store or RFC 3339 timestamp, both UTC, as the browser's local time. */
export function when(t?: string | null) {
	if (!t) return '';
	const iso = t.replace(' ', 'T');
	return new Date(iso.endsWith('Z') ? iso : iso + 'Z').toLocaleString();
}

/** A summary line's label or value, which the tracker shouts, in sentence case. */
export const sentence = (s: string) => s.charAt(0).toUpperCase() + s.slice(1).toLowerCase();

/** An RFC 3339 timestamp without its T and Z. */
export const date = (t?: string | null) => (t ? t.replace('T', ' ').replace('Z', '') : '');

export const num = (v: number | null | undefined, digits: number) =>
	v == null ? '-' : v.toFixed(digits);
