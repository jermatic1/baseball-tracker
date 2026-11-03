<script lang="ts">
	// What a list shows when it has nothing: a title and the way to fill it.
	let { title, children }: { title: string; children?: import('svelte').Snippet } = $props();
</script>

<div class="px-4 py-10 text-center text-sm opacity-70">
	<div class="text-base-content text-base font-medium">{title}</div>
	{#if children}<div class="mt-1">{@render children()}</div>{/if}
</div>
