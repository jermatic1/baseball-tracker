// The tracker embeds the built app and serves index.html for every page
// path, so nothing renders on a server and nothing is prerendered.
export const ssr = false;
export const prerender = false;

/** The game modules with a page, for the navigation; none while the tracker is away. */
export async function load({ fetch }) {
	try {
		const r = await fetch('/api/games');
		return { games: r.ok ? ((await r.json()) as string[]) : [] };
	} catch {
		return { games: [] as string[] };
	}
}
