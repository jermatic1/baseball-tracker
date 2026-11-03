<script lang="ts">
	import './layout.css';
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { afterNavigate } from '$app/navigation';
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
	let current = $derived(links.find(([href]) => here(href))?.[1] ?? 'Tracker');

	onMount(() => {
		start();
	});
	// The dropdown is open while its button or list has focus, so leaving a
	// page closes it.
	afterNavigate(() => (document.activeElement as HTMLElement | null)?.blur());

	// An error from the tracker shows until the next one or a few seconds pass.
	$effect(() => {
		if (!live.error) return;
		const timer = setTimeout(() => (live.error = ''), 8000);
		return () => clearTimeout(timer);
	});
</script>

<header class="bg-base-100/90 border-base-300 sticky top-0 z-20 border-b backdrop-blur">
	<div class="navbar mx-auto min-h-14 max-w-5xl px-2 sm:px-4">
		<div class="navbar-start w-auto flex-1 gap-1">
			<div class="dropdown sm:hidden">
				<div tabindex="0" role="button" class="btn btn-ghost btn-square text-base-content" aria-label="menu">
					<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
						<path d="M4 6h16M4 12h16M4 18h16" />
					</svg>
				</div>
				<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
				<ul tabindex="0" class="menu dropdown-content menu-lg surface rounded-box z-30 mt-2 w-60 p-2 shadow-lg">
					{#each links as [href, label] (href)}
						<li><a {href} class={here(href) ? 'menu-active' : ''}>{label}</a></li>
					{/each}
				</ul>
			</div>
			<span class="text-base font-semibold sm:hidden">{current}</span>
			<a href="/" class="hidden px-2 text-base font-semibold sm:block">Tracker</a>
			<ul class="menu menu-horizontal hidden gap-2 p-0 sm:flex">
				{#each links as [href, label] (href)}
					<li><a {href} class={here(href) ? 'menu-active' : ''}>{label}</a></li>
				{/each}
			</ul>
		</div>
		<div class="navbar-end w-auto gap-2 px-2 text-xs opacity-70">
			<span class="status {live.connected ? 'status-success' : 'status-error'}" aria-hidden="true"></span>
			{live.link}
		</div>
	</div>
</header>

<main class="mx-auto max-w-5xl px-4 py-5 pb-10">
	{@render children()}
</main>

{#if live.error}
	<div class="toast toast-center z-20">
		<div role="alert" class="alert alert-error">{live.error}</div>
	</div>
{/if}
