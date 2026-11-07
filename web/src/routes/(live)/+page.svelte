<script lang="ts">
	import { live, send, userName, type HitRecord } from '#lib/socket.svelte.ts';
	import { getJson } from '#lib/api.ts';
	import { num, sentence, when, type SummaryLine } from '#lib/format.ts';
	import type { Location, Zone } from '#lib/socket.svelte.ts';
	import Stat from '#lib/components/Stat.svelte';
	import Segmented from '#lib/components/Segmented.svelte';
	import Empty from '#lib/components/Empty.svelte';
	import ZonePlot from '#lib/components/ZonePlot.svelte';
	import Stepper from '#lib/components/Stepper.svelte';

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

	// The pitches shown, newest first, and the zone the latest was judged by.
	let pitches = $derived(live.events.flatMap((e) => (e.record.pitch ? [e.record.pitch] : [])));

	// The aim tool for pitching from closer than the simulated mound: where
	// to throw at the target, for a release speed, from the tracker.
	interface Aim {
		measured_from: number;
		actual_distance: number;
		simulated_distance: number;
		aim: { release_height: number; target_behind: number; height: number; zone: Zone } | null;
	}
	let aimSpeed = $state(45);
	let aim = $state<Aim | null>(null);
	let pitching = $derived(live.session ? live.session.activity === 'pitching' : activity === 'pitching');
	let pitcher = $derived(live.session ? live.session.user : user);
	$effect(() => {
		if (!pitching || !pitcher) return;
		void live.events.length;
		getJson<Aim>(`/api/aim?speed=${aimSpeed}&user=${pitcher}`)
			.then((a) => (aim = a))
			.catch(() => (aim = null));
	});
	let closer = $derived(!!aim && aim.actual_distance < aim.simulated_distance);

	// Status as badges: capture, detector, clips, and a model that needs attention.
	let chips = $derived.by((): [string, string][] => {
		const m = live.status;
		if (!m) return [];
		const out: [string, string][] = [[m.capturing ? 'capturing' : 'idle', m.capturing ? 'badge-soft badge-success' : '']];
		const d = m.detector;
		if (d) out.push([`detector ${d.state} · ${d.rate.toFixed(0)} fps`, ''], [`${d.clips} clips`, '']);
		const model = m.model;
		if (model?.loading) out.push(['loading model', 'badge-soft badge-warning']);
		else if (!model?.file) out.push(['no model', 'badge-soft badge-warning']);
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

{#if pitches.length}
	<div class="card surface mt-4">
		<div class="card-body gap-4 p-3 sm:flex-row sm:items-start sm:p-4">
			<div class="mx-auto w-48 shrink-0 sm:mx-0"><ZonePlot zone={pitches[0].zone} {pitches} /></div>
			<div class="min-w-0 flex-1 text-sm">
				<div class="caption mb-1">Strike zone</div>
				<p class="opacity-70">From behind the plate. Hollow is where the ball crossed the plate; filled is the same throw from the simulated distance, green for a strike and red for a ball.</p>
			</div>
		</div>
	</div>
{/if}

{#if pitching && closer && aim}
	<div class="card surface mt-4">
		<div class="card-body gap-4 p-3 sm:flex-row sm:items-start sm:p-4">
			{#if aim.aim}
				<div class="mx-auto w-48 shrink-0 sm:mx-0"><ZonePlot zone={aim.aim.zone} pitches={[]} aim={{ side: 0, height: aim.aim.height }} /></div>
			{/if}
			<div class="min-w-0 flex-1">
				<div class="caption mb-1">Aim</div>
				{#if aim.aim}
					{@const z = aim.aim.zone}
					<div class="flex items-baseline gap-2">
						<span class="text-2xl font-semibold tabular-nums">{aim.aim.height.toFixed(0)}</span>
						<span class="text-sm opacity-70">in high at the target, {aim.aim.target_behind} ft behind the plate</span>
					</div>
					<p class="mt-1 text-sm opacity-70">
						The zone there is a box {z.width.toFixed(0)} in wide from {z.bottom.toFixed(0)} to {z.top.toFixed(0)} in up, centred on the plate.
						Release {aim.aim.release_height.toFixed(0)} in high, {aim.measured_from ? `measured from ${aim.measured_from} pitches` : 'assumed until pitches are measured'}.
					</p>
				{:else}
					<p class="text-sm opacity-70">No aim point: the target hangs past the simulated plate or the throw cannot reach the zone.</p>
				{/if}
				<div class="mt-3 max-w-xs">
					<Stepper label="Release speed" value={`${aimSpeed} mph`} step="1 mph" onstep={(d) => (aimSpeed += d)} />
				</div>
			</div>
		</div>
	</div>
{/if}

{#if chips.length}
	<div class="mt-4 flex flex-wrap gap-1.5">
		{#each chips as [text, tone] (text)}<span class="badge badge-sm {tone || 'badge-ghost'}">{text}</span>{/each}
	</div>
{/if}

<h2>Events</h2>
{#if !live.events.length}
	<div class="surface rounded-box"><Empty title="Nothing measured yet">Every hit and pitch shows here as it happens.</Empty></div>
{:else}
	<ul class="rows sm:hidden">
			{#each live.events as { seq, clip, record: r } (seq)}
				{@const [value, unit] = headline(r)}
				<li class="p-4">
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
	<div class="surface rounded-box hidden overflow-hidden sm:block">
		<table class="table">
			<thead>
				<tr>
					<th>Clip</th><th>Kind</th><th class="text-right">Exit mph</th><th class="text-right">Launch</th><th class="text-right">Spray</th>
					<th class="text-right">Carry ft</th><th class="text-right">Pitch mph</th><th>Location</th><th>Call</th><th class="text-right">Samples</th>
				</tr>
			</thead>
			<tbody class="tabular-nums">
				{#each live.events as { seq, clip, record: r } (seq)}
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
	</div>
{/if}

<h2>Past sessions</h2>
<ul class="rows">
	{#each history.sessions as s (s.id)}
		<li class="flex flex-col gap-3 p-4 sm:flex-row sm:items-start">
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
