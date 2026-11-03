<script lang="ts">
	// The pages that need a tracker: shown once one is connected and has sent
	// its status, so each loads on connect and again after a reconnect.
	import { live, reconnect } from '#lib/socket.svelte.ts';

	let { children } = $props();
</script>

{#if live.link === 'connected' && live.status}
	{@render children()}
{:else if live.link === 'offline'}
	<div class="card surface mt-4 max-w-lg">
		<div class="card-body">
			<h2 class="card-title mt-0 mb-0">
				<span class="status status-error" aria-hidden="true"></span>
				Not connected
			</h2>
			<p class="opacity-70">No tracker is answering at {location.host}.</p>
			<div class="card-actions"><button class="btn" onclick={reconnect}>Retry</button></div>
		</div>
	</div>
{:else}
	<p class="mt-4 flex items-center gap-2 text-sm opacity-70">
		<span class="loading loading-spinner loading-sm"></span>
		Connecting to the tracker
	</p>
{/if}
