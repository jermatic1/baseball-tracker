<script lang="ts">
	// The clips of a session as a strip on phones and a rail on wider screens.
	// `marked` clips show struck through.
	let {
		clips,
		current,
		marked = new Set<string>(),
		onselect
	}: {
		clips: string[];
		current: string | null;
		marked?: Set<string>;
		onselect: (id: string) => void;
	} = $props();

	function reveal(el: HTMLElement, id: string) {
		$effect(() => {
			if (id === current) el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
		});
	}
</script>

<nav
	aria-label="clips"
	class="surface rounded-box min-w-0 overflow-x-auto md:max-h-[calc(100dvh-9rem)] md:overflow-x-hidden md:overflow-y-auto"
>
	<ul class="menu menu-horizontal w-max flex-nowrap md:menu-vertical md:w-full">
		{#each clips as id (id)}
			<li>
				<button
					use:reveal={id}
					class="tabular-nums {id === current ? 'menu-active' : ''} {marked.has(id) ? 'line-through opacity-50' : ''}"
					aria-current={id === current ? 'true' : undefined}
					onclick={() => onselect(id)}>{id}</button
				>
			</li>
		{:else}
			<li class="menu-disabled"><span>No clips</span></li>
		{/each}
	</ul>
</nav>
