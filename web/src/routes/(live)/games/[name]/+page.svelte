<script lang="ts">
	import { page } from '$app/state';
	import Challenges from '#lib/games/Challenges.svelte';
</script>

{#if page.params.name === 'challenges'}
	<Challenges />
{:else}
	<h1>{page.params.name}</h1>
	<p class="mt-4 opacity-70">This game module has no page.</p>
{/if}
