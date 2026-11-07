<script lang="ts">
	import './layout.css';
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { afterNavigate, goto } from '$app/navigation';
	import { live, start } from '#lib/socket.svelte.ts';

	let { children, data } = $props();

	const fixed: [string, string][] = [
		['/', 'Tracker'],
		['/setup', 'Setup'],
		['/sessions', 'Sessions'],
		['/users', 'Users'],
		['/train', 'Train']
	];
	let links = $derived([
		...fixed,
		...data.games.map((g): [string, string] => [`/games/${g}`, g[0].toUpperCase() + g.slice(1)])
	]);
	const here = (href: string) =>
		href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);
	// The page's name, which the bar carries on phones in place of its heading.
	let current = $derived.by(() => {
		const path = page.url.pathname;
		if (path.endsWith('/label')) return 'Label';
		if (path.endsWith('/playback')) return 'Playback';
		return links.find(([href]) => here(href))?.[1] ?? 'Tracker';
	});
	// The phone menu is a drawer; it closes on navigation and Escape.
	let menu = $state(false);

	// The Android app's Back: close the menu, else return to the tracker, else
	// report false so the app exits.
	function back() {
		if (menu) menu = false;
		else if (page.url.pathname !== '/') goto('/');
		else return false;
		return true;
	}

	onMount(() => {
		start();
		Object.assign(window, { trackerBack: back });
	});
	afterNavigate(() => (menu = false));

	// An error from the tracker shows until the next one or a few seconds pass.
	$effect(() => {
		if (!live.error) return;
		const timer = setTimeout(() => (live.error = ''), 8000);
		return () => clearTimeout(timer);
	});
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && (menu = false)} />

<div class="drawer">
	<input id="menu" type="checkbox" class="drawer-toggle" bind:checked={menu} />
	<div class="drawer-content">
		<header class="bg-base-100/90 border-base-300 sticky top-0 z-20 border-b backdrop-blur">
			<div class="navbar mx-auto min-h-14 max-w-5xl px-2 sm:px-4">
				<div class="navbar-start w-auto flex-1 gap-1">
					<label for="menu" class="btn btn-ghost btn-square text-base-content sm:hidden" aria-label="menu">
						<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
							<path d="M4 6h16M4 12h16M4 18h16" />
						</svg>
					</label>
					<span class="text-base font-semibold sm:hidden">{current}</span>
					<a href="/" class="hidden px-2 text-base font-semibold sm:block">Tracker</a>
					<ul class="menu menu-horizontal hidden gap-2 p-0 sm:flex">
						{#each links as [href, label] (href)}
							<li><a {href} class={here(href) ? 'menu-active' : ''}>{label}</a></li>
						{/each}
					</ul>
				</div>
				<div class="navbar-end w-auto gap-2 px-2 text-xs opacity-70">
					<span class="status {live.connected ? 'status-success' : live.link === 'connecting' ? 'status-warning' : 'status-error'}" aria-hidden="true"></span>
					{live.link}
				</div>
			</div>
		</header>

		<main class="mx-auto max-w-5xl px-4 py-5 pb-10 max-sm:pt-3">
			{@render children()}
		</main>
	</div>

	<div class="drawer-side z-30 sm:hidden">
		<label for="menu" class="drawer-overlay" aria-label="close menu"></label>
		<nav class="bg-base-200 border-base-300 flex min-h-full w-72 max-w-[85vw] flex-col border-r shadow-2xl">
			<div class="border-base-300 flex h-14 items-center justify-between border-b pr-2 pl-4">
				<span class="text-base font-semibold">Tracker</span>
				<label for="menu" class="btn btn-ghost btn-square btn-sm" aria-label="close menu">
					<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
						<path d="M6 6l12 12M18 6L6 18" />
					</svg>
				</label>
			</div>
			<ul class="menu menu-lg w-full gap-1 p-2">
				{#each links as [href, label] (href)}
					<li><a {href} class={here(href) ? 'menu-active' : ''}>{label}</a></li>
				{/each}
			</ul>
		</nav>
	</div>
</div>

{#if live.error}
	<div class="toast toast-center z-20">
		<div role="alert" class="alert alert-error">{live.error}</div>
	</div>
{/if}
