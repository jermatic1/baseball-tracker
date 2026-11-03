<script lang="ts">
	// A setting with its current value and buttons to step it down or up.
	let {
		label,
		value,
		step,
		onstep
	}: { label: string; value: string; step: string; onstep: (delta: 1 | -1) => void } = $props();
</script>

<div class="bg-base-300/50 rounded-field flex items-center gap-2 py-2 pr-2 pl-3">
	<div class="min-w-0 flex-1">
		<div class="caption">{label}</div>
		<div class="text-lg tabular-nums">{value}</div>
	</div>
	<div class="join">
		<button class="btn join-item btn-sm btn-square text-base" aria-label="{label} down by {step}" onclick={() => onstep(-1)}>−</button>
		<button class="btn join-item btn-sm btn-square text-base" aria-label="{label} up by {step}" onclick={() => onstep(1)}>+</button>
	</div>
</div>
