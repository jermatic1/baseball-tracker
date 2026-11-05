<script lang="ts">
	// The strike zone from behind the plate with where pitches crossed. A
	// hollow dot is where the ball really crossed the front of the plate; the
	// filled dot, coloured by the call, is where the same throw crosses from
	// the simulated mound distance. They coincide when nothing is projected.
	import type { Location, Zone } from '#lib/socket.svelte.ts';

	interface Pitch {
		location: Location;
		simulated_location: Location;
		call: string;
	}
	let { zone, pitches }: { zone: Zone; pitches: Pitch[] } = $props();

	// Inches across and up, drawn with the ground at the bottom.
	const half = 24;
	let top = $derived(Math.max(zone.top + 12, 54));
	const x = (side: number) => side + half;
	let y = $derived((height: number) => top - height);
	const ball = 1.45;
	const same = (p: Pitch) => p.location.side === p.simulated_location.side && p.location.height === p.simulated_location.height;
	let latest = $derived(pitches[0]);
</script>

<svg viewBox="0 0 {half * 2} {top}" class="w-full" role="img" aria-label="strike zone with pitch locations">
	<line x1="0" y1={top} x2={half * 2} y2={top} class="stroke-base-content/40" stroke-width="0.5" />
	<rect x={x(-zone.width / 2)} y={y(0) - 1} width={zone.width} height="1" class="fill-base-content/40" />
	<rect
		x={x(-zone.width / 2)}
		y={y(zone.top)}
		width={zone.width}
		height={zone.top - zone.bottom}
		class="fill-base-content/5 stroke-base-content/70"
		stroke-width="0.6"
	/>
	{#each [...pitches].reverse() as p, k (k)}
		{@const old = p !== latest}
		{#if !same(p)}
			{#if !old}
				<line
					x1={x(p.location.side)}
					y1={y(p.location.height)}
					x2={x(p.simulated_location.side)}
					y2={y(p.simulated_location.height)}
					class="stroke-base-content/40"
					stroke-width="0.4"
				/>
			{/if}
			<circle
				cx={x(p.location.side)}
				cy={y(p.location.height)}
				r={ball}
				class="fill-none stroke-base-content {old ? 'opacity-30' : 'opacity-80'}"
				stroke-width="0.5"
			/>
		{/if}
		<circle
			cx={x(p.simulated_location.side)}
			cy={y(p.simulated_location.height)}
			r={ball}
			class="{p.call === 'strike' ? 'fill-success' : 'fill-error'} {old ? 'opacity-40' : ''}"
			stroke-width={old ? 0 : 0.5}
			stroke="white"
		/>
	{/each}
</svg>
