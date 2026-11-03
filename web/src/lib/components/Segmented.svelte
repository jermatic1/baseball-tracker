<script lang="ts">
	// One choice among a few, as joined buttons.
	let {
		options,
		value = $bindable(),
		onchange,
		small = false
	}: {
		options: [string, string][];
		value: string;
		onchange?: (value: string) => void;
		small?: boolean;
	} = $props();
</script>

<div class="join">
	{#each options as [key, label] (key)}
		<button
			class="btn join-item {small ? 'btn-sm' : ''} {value === key ? 'btn-selected' : ''}"
			aria-pressed={value === key}
			onclick={() => {
				value = key;
				onchange?.(key);
			}}>{label}</button
		>
	{/each}
</div>
