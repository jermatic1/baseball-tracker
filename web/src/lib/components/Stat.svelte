<script lang="ts">
	// A tile with a label above a value and a note under it. The note line is
	// always there so tiles in a row match and their values line up.
	let { label, value, note = '' }: { label: string; value: string; note?: string } = $props();
</script>

<div class="stat surface rounded-box h-full min-w-0 px-4 py-3">
	<div class="stat-title truncate">{label}</div>
	<div class="stat-value truncate text-2xl">{value}</div>
	<div class="stat-desc min-h-4 truncate">{note}</div>
</div>
