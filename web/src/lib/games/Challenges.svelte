<script lang="ts">
	import { onMount } from 'svelte';
	import { live, on, type Message } from '#lib/socket.svelte.ts';
	import Empty from '#lib/components/Empty.svelte';

	interface Batter {
		id: string;
		name: string;
		number: number;
		bats: string;
	}
	interface Outcome {
		goal?: string;
		swing?: string;
		batted?: string;
	}

	let roster = $state<Batter[]>([]);
	let current = $state<{ id: string; goal: string } | null>(null);
	let outcomes = $state<Record<string, Outcome>>({});
	let results = $state<{ batter: string; result: string }[]>([]);

	const batterName = (id: string) => roster.find((b) => b.id === id)?.name ?? id;
	let pitches = $derived(live.events.filter((e) => e.record.kind === 'pitch' && e.record.pitch));

	function body(b: Message) {
		if (b.type === 'roster') {
			roster = b.batters;
		} else if (b.type === 'batter') {
			// The previous batter's result arrives from the front end; show the change of batter.
			if (current && b.id !== current.id) results = [{ batter: batterName(current.id), result: 'done' }, ...results];
			current = b;
		} else if (b.type === 'verdict' && pitches[0]) {
			(outcomes[pitches[0].clip] ??= {}).goal = b.met ? 'met' : 'missed';
		} else if (b.type === 'swing' && pitches[0]) {
			const o = (outcomes[pitches[0].clip] ??= {});
			o.swing = b.swing;
			o.batted = b.record
				? `${b.record.exit_velocity.toFixed(0)} mph, ${b.record.launch_angle.toFixed(0)}°, ${b.record.hit.carry.toFixed(0)} ft`
				: '';
		}
	}

	onMount(() => {
		const kept = live.gameState.challenges ?? {};
		if (kept.roster) body(kept.roster);
		if (kept.batter) body(kept.batter);
		return on('game', (m) => m.game === 'challenges' && body(m.body));
	});
</script>

<h1>Challenges</h1>

<div class="card surface mt-4">
	<div class="card-body">
		{#if !live.session}
			<Empty title="No session">Start one on the <a href="/" class="link">Tracker</a> page.</Empty>
		{:else}
			<div class="flex flex-wrap items-center gap-3">
				<span class="badge badge-soft badge-success badge-sm">session {live.session.id}</span>
				<span class="text-sm opacity-70">{live.session.activity} · {live.session.mode}</span>
			</div>
			{#if current}
				<div class="mt-2">
					<div class="caption">Up</div>
					<div class="text-lg font-semibold">{batterName(current.id)}</div>
					<div class="text-sm opacity-70">Goal: {current.goal}</div>
				</div>
			{/if}
			{#if roster.length}
				<div class="mt-2">
					<div class="caption">Lineup</div>
					<div class="mt-1 flex flex-wrap gap-1.5">
						{#each roster as b (b.id)}
							<span class="badge badge-sm {current?.id === b.id ? 'badge-soft badge-success' : 'badge-ghost'}">{b.name} #{b.number} ({b.bats[0].toUpperCase()})</span>
						{/each}
					</div>
				</div>
			{/if}
		{/if}
	</div>
</div>

<h2>Pitches</h2>
<div class="surface rounded-box overflow-hidden">
	{#if !pitches.length}
		<Empty title="No pitches yet">Each pitch shows here with its call and the batter's result.</Empty>
	{:else}
		<ul class="list sm:hidden">
			{#each pitches as { clip, record: r } (clip)}
				{@const l = r.pitch!.simulated_location}
				{@const o = outcomes[clip] ?? {}}
				<li class="list-row block">
					<div class="flex items-center justify-between text-xs opacity-60">
						<span>Clip {clip}</span><span>{r.pitch!.call}</span>
					</div>
					<div class="mt-1 flex items-baseline gap-2">
						<span class="text-2xl font-semibold tabular-nums">{r.exit_velocity?.toFixed(1)}</span>
						<span class="text-sm opacity-70">mph at {l.side.toFixed(0)}, {l.height.toFixed(0)} in</span>
					</div>
					<dl class="mt-2 grid grid-cols-3 gap-x-3 gap-y-1.5">
						<div class="fact"><dt class="caption">Goal</dt><dd>{o.goal ?? '…'}</dd></div>
						<div class="fact"><dt class="caption">Swing</dt><dd>{o.swing ?? '…'}</dd></div>
						<div class="fact min-w-0"><dt class="caption">Batted</dt><dd class="truncate">{o.batted ?? '…'}</dd></div>
					</dl>
				</li>
			{/each}
		</ul>
		<table class="table hidden sm:table">
			<thead>
				<tr><th>Clip</th><th class="text-right">mph</th><th>Location</th><th>Call</th><th>Goal</th><th>Swing</th><th>Batted</th></tr>
			</thead>
			<tbody class="tabular-nums">
				{#each pitches as { clip, record: r } (clip)}
					{@const l = r.pitch!.simulated_location}
					{@const o = outcomes[clip] ?? {}}
					<tr>
						<td>{clip}</td>
						<td class="text-right">{r.exit_velocity?.toFixed(1)}</td>
						<td>{l.side.toFixed(0)}, {l.height.toFixed(0)} in</td>
						<td>{r.pitch!.call}</td>
						<td>{o.goal ?? ''}</td>
						<td>{o.swing ?? ''}</td>
						<td>{o.batted ?? ''}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	{/if}
</div>

{#if results.length}
	<h2>Results</h2>
	<div class="surface rounded-box max-w-md overflow-hidden">
		<table class="table">
			<thead><tr><th>Batter</th><th>Result</th></tr></thead>
			<tbody>
				{#each results as r, i (i)}
					<tr><td>{r.batter}</td><td>{r.result}</td></tr>
				{/each}
			</tbody>
		</table>
	</div>
{/if}

<p class="mt-6 text-sm opacity-60">
	Batters, goals, and plans come from <code>challenges.toml</code> in the data directory.
</p>
