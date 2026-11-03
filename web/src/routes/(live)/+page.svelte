<script lang="ts">
	import { live, send, userName, type HitRecord } from '#lib/socket.svelte.ts';
	import { getJson } from '#lib/api.ts';
	import { num, sentence, when, type SummaryLine } from '#lib/format.ts';
	import type { Location } from '#lib/socket.svelte.ts';
	import Stat from '#lib/components/Stat.svelte';
	import Segmented from '#lib/components/Segmented.svelte';
	import Empty from '#lib/components/Empty.svelte';

	interface History {
		sessions: {
			id: number;
			user: string;
			activity: string;
			mode: string;
			started: string;
			ended: string | null;
			lines: SummaryLine[];
		}[];
	}

	let user = $state('');
	let activity = $state('hitting');
	let mode = $state('practice');
	let history = $state<History>({ sessions: [] });

	// Every user's stats and every session, again whenever either changes.
	$effect(() => {
		void live.users;
		void live.session;
		getJson<History>('/api/history')
			.then((h) => (history = h))
			.catch(() => {});
	});
	// Keep a valid user selected.
	$effect(() => {
		if (live.users.length && !live.users.some((u) => u.id === user)) user = live.users[0].id;
	});
	const mph = (v: number | null) => (v == null ? '-' : v.toFixed(1));
	// Label, value, and note per tile: the live counts during a session, or
	// the summary lines after one.
	let tiles = $derived.by((): [string, string, string][] => {
		const s = live.stats;
		if (s) {
			return [
				['Pitches', String(s.pitches), ''],
				['Strikes', String(s.strikes), ''],
				['Pitch mph', mph(s.avg_pitch_mph), `top ${mph(s.top_pitch_mph)}`],
				['Exit mph', mph(s.avg_exit_mph), `top ${mph(s.top_exit_mph)}`],
				['Longest ft', s.longest_ft == null ? '-' : s.longest_ft.toFixed(0), '']
			];
		}
		return (live.summary?.lines ?? []).map((l) => [sentence(l.label), l.value.toLowerCase(), '']);
	});

	// Status as badges: capture, detector, clips, and the model.
	let chips = $derived.by((): [string, string][] => {
		const m = live.status;
		if (!m) return [];
		const out: [string, string][] = [[m.capturing ? 'capturing' : 'idle', m.capturing ? 'badge-soft badge-success' : '']];
		const d = m.detector;
		if (d) out.push([`detector ${d.state} · ${d.rate.toFixed(0)} fps`, ''], [`${d.clips} clips`, '']);
		const model = m.model;
		if (model?.loading) out.push(['loading model', 'badge-soft badge-warning']);
		else if (model?.file) out.push([`${model.file} ${model.sha1?.slice(0, 7) ?? ''}`.trim(), '']);
		else out.push(['no model', 'badge-soft badge-warning']);
		if (model && model.untrained > 0) out.push([`${model.untrained} sessions to train on`, 'badge-soft badge-warning']);
		const others = m.clients.filter((c) => c !== 'page' && !c.endsWith(' page'));
		if (others.length) out.push([`simulators ${others.join(', ')}`, '']);
		return out;
	});

	const loc = (l?: Location) => (l ? `${l.side.toFixed(0)}, ${l.height.toFixed(0)} in` : '-');
	const kind = (r: HitRecord) => r.kind + (r.hit_type ? ` · ${r.hit_type.replace('_', ' ')}` : '');
	// The number a card leads with and the rest of its measurements.
	function headline(r: HitRecord): [string, string] {
		return r.kind === 'pitch' ? [num(r.pitch_velocity, 1), 'mph pitch'] : [num(r.exit_velocity, 1), 'mph exit'];
	}
	function details(r: HitRecord): [string, string][] {
		const out: [string, string][] = [];
		if (r.launch_angle != null) out.push(['launch', `${r.launch_angle.toFixed(1)}°`]);
		if (r.spray_angle != null) out.push(['spray', `${r.spray_angle.toFixed(1)}°`]);
		if (r.hit) out.push(['carry', `${r.hit.carry.toFixed(0)} ft`]);
		if (r.kind !== 'pitch' && r.pitch_velocity != null) out.push(['pitch', `${r.pitch_velocity.toFixed(1)} mph`]);
		if (r.pitch) {
			out.push(['release / plate', `${r.pitch.release_velocity.toFixed(1)} / ${r.pitch.plate_velocity.toFixed(1)}`]);
			out.push(['location', loc(r.pitch.location)], ['simulated', loc(r.pitch.simulated_location)], ['call', r.pitch.call]);
		}
		if (r.contact) out.push(['contact', r.contact.kind]);
		out.push(['samples', String(r.samples)]);
		return out;
	}

	const startSession = () => send({ type: 'start_session', user, activity, mode });
	const endSession = () => live.session && send({ type: 'end_session', session: live.session.id });

	// With no trained detector the page offers a hand recording to label.
	let noDetector = $derived(!!live.status?.model && !live.status.model.file);
	function record() {
		live.recorded = null;
		send({ type: 'record', action: 'start' });
	}
	const stopRecord = () => send({ type: 'record', action: 'stop' });
</script>

<h1>Tracker</h1>

{#if noDetector}
	<div role="status" class="card surface mt-4">
		<div class="card-body gap-3">
			<p class="text-warning">
				No detector yet. Record a few hits, click the ball through each one on the label page,
				then train from the <a href="/train" class="link">Train</a> page.
			</p>
			<div class="flex items-center justify-between gap-2">
				<span class="min-w-0 text-sm">
					{#if live.status?.recording}
						<span class="opacity-70">{live.status.camera ? 'Recording' : 'Starting the camera'}</span>
					{:else if live.recorded}
						<a class="btn btn-sm btn-primary" href="/sessions/{live.recorded.session}/label?clip={live.recorded.clip}">Label clip {live.recorded.clip}</a>
					{/if}
				</span>
				{#if live.status?.recording}
					<button class="btn btn-sm" onclick={stopRecord}>Stop</button>
				{:else}
					<button class="btn btn-sm" disabled={!live.status} onclick={record}>Record</button>
				{/if}
			</div>
		</div>
	</div>
{/if}

<div class="card surface mt-4">
	<div class="card-body">
		{#if live.session}
			<div class="flex flex-wrap items-center gap-3">
				<span class="badge badge-soft badge-success badge-sm"><span class="status status-success status-xs"></span>live</span>
				<div class="min-w-0 flex-1">
					<div class="text-lg font-semibold">Session {live.session.id}</div>
					<div class="text-sm opacity-70">
						{userName(live.session.user)} · {live.session.activity} · {live.session.mode}
					</div>
				</div>
				<button class="btn w-full sm:w-auto" onclick={endSession}>End session</button>
			</div>
		{:else}
			{#if !live.users.length}
				<p class="opacity-70">Add a user on the <a href="/users" class="link">Users</a> page to start a session.</p>
			{:else}
				<div class="grid gap-4 sm:grid-cols-[1fr_auto] sm:items-end">
					<div class="grid gap-3 sm:grid-cols-3">
						<div>
							<div class="caption mb-1">User</div>
							<select bind:value={user} class="select w-full">
								{#each live.users as u (u.id)}
									<option value={u.id}>{u.name}</option>
								{/each}
							</select>
						</div>
						<div>
							<div class="caption mb-1">Activity</div>
							<Segmented options={[['hitting', 'Hitting'], ['pitching', 'Pitching']]} bind:value={activity} />
						</div>
						<div>
							<div class="caption mb-1">Mode</div>
							<Segmented options={[['practice', 'Practice'], ['game', 'Game']]} bind:value={mode} />
						</div>
					</div>
					<button class="btn btn-primary" disabled={!user} onclick={startSession}>Start session</button>
				</div>
			{/if}
		{/if}
	</div>
</div>

{#if tiles.length}
	{#if !live.stats && live.summary}
		<h2 class="mt-5 mb-2">Session {live.summary.session} summary · {userName(live.summary.user)}</h2>
	{/if}
	<div class="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-[repeat(auto-fit,minmax(9rem,1fr))]">
		{#each tiles as [label, value, note] (label)}
			<div class="odd:last:col-span-2 sm:odd:last:col-span-1"><Stat {label} {value} {note} /></div>
		{/each}
	</div>
{/if}

{#if chips.length}
	<div class="mt-4 flex flex-wrap gap-1.5">
		{#each chips as [text, tone] (text)}<span class="badge badge-sm {tone || 'badge-ghost'}">{text}</span>{/each}
	</div>
{/if}

<h2>Events</h2>
<div class="surface rounded-box overflow-hidden">
	{#if !live.events.length}
		<Empty title="Nothing measured yet">Every hit and pitch shows here as it happens.</Empty>
	{:else}
		<ul class="list sm:hidden">
			{#each live.events as { clip, record: r } (clip)}
				{@const [value, unit] = headline(r)}
				<li class="list-row block">
					<div class="flex items-center justify-between text-xs opacity-60">
						<span>Clip {clip}</span><span>{kind(r)}</span>
					</div>
					<div class="mt-1 flex items-baseline gap-2">
						<span class="text-2xl font-semibold tabular-nums">{value}</span>
						<span class="text-sm opacity-70">{unit}</span>
					</div>
					<dl class="mt-2 grid grid-cols-3 gap-x-3 gap-y-1.5">
						{#each details(r) as [k, v] (k)}
							<div class="fact min-w-0"><dt class="caption">{k}</dt><dd class="truncate">{v}</dd></div>
						{/each}
					</dl>
				</li>
			{/each}
		</ul>
		<table class="table hidden sm:table">
			<thead>
				<tr>
					<th>Clip</th><th>Kind</th><th class="text-right">Exit mph</th><th class="text-right">Launch</th><th class="text-right">Spray</th>
					<th class="text-right">Carry ft</th><th class="text-right">Pitch mph</th><th>Location</th><th>Call</th><th class="text-right">Samples</th>
				</tr>
			</thead>
			<tbody class="tabular-nums">
				{#each live.events as { clip, record: r } (clip)}
					<tr>
						<td>{clip}</td>
						<td>{kind(r)}</td>
						<td class="text-right">{num(r.exit_velocity, 1)}</td>
						<td class="text-right">{r.launch_angle == null ? '-' : `${r.launch_angle.toFixed(1)}°`}</td>
						<td class="text-right">{r.spray_angle == null ? '-' : `${r.spray_angle.toFixed(1)}°`}</td>
						<td class="text-right">{r.hit ? r.hit.carry.toFixed(0) : '-'}</td>
						<td class="text-right">{num(r.pitch_velocity, 1)}</td>
						<td>{loc(r.pitch?.location)}</td>
						<td>{r.pitch ? r.pitch.call : '-'}</td>
						<td class="text-right">{r.samples}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	{/if}
</div>

<h2>Past sessions</h2>
<ul class="list surface rounded-box">
	{#each history.sessions as s (s.id)}
		<li class="list-row flex flex-col gap-3 sm:flex-row sm:items-start">
			<div class="sm:w-48">
				<div class="font-semibold">Session {s.id} · {userName(s.user)}</div>
				<div class="text-xs opacity-60">{s.activity} · {s.mode}</div>
				<div class="text-xs opacity-60">{when(s.started)}{s.ended ? '' : ' · active'}</div>
			</div>
			<dl class="grid flex-1 grid-cols-2 gap-x-3 gap-y-2 sm:grid-cols-4">
				{#each s.lines as l (l.label)}
					<div class="fact"><dt class="caption">{sentence(l.label)}</dt><dd>{l.value.toLowerCase()}</dd></div>
				{/each}
			</dl>
		</li>
	{:else}
		<li><Empty title="No sessions yet">Finished sessions and their summaries land here.</Empty></li>
	{/each}
</ul>
