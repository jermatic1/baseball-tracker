<script lang="ts">
	import { onMount } from 'svelte';
	import { getJson, sendJson } from '#lib/api.ts';
	import { date, type ModelStatus } from '#lib/format.ts';
	import Empty from '#lib/components/Empty.svelte';

	interface SessionRow {
		id: string;
		ball: number;
		empty: number;
		clicks: number;
		error?: string;
	}
	interface Result {
		candidate?: Record<string, number>;
		current?: Record<string, number>;
		epochs?: number;
		frames?: number;
	}
	interface Job {
		phase: string;
		built?: { train: number; val: number; holdout: number };
		log?: string;
		error?: string;
		result?: Result;
		promoted?: string;
		model?: ModelStatus;
	}
	interface Overview {
		server: string;
		model: string;
		health?: { ok: boolean; busy: boolean };
		health_error?: string;
		record?: { sessions: string[] };
		include: SessionRow[];
		holdout: SessionRow[];
		job: Job;
	}

	const ACTIVE = ['building', 'sending', 'queued', 'running'];
	// Label, result key, and whether more is better.
	const SCORES: [string, string, boolean][] = [
		['Recall', 'recall', true],
		['False detections per empty frame', 'false_per_empty', false],
		['mAP50', 'map50', true]
	];

	let overview = $state<Overview | null>(null);
	let job = $state<Job>({ phase: 'idle' });
	let error = $state('');
	let promoting = $state(false);
	let timer: ReturnType<typeof setInterval> | null = null;
	let wasLoading = false;
	let log = $state<HTMLDivElement>();

	let server = $derived(overview?.server ?? '');
	let serverBusy = $derived(!!overview?.health?.busy);
	let trained = $derived(overview?.record?.sessions ?? []);
	let loading = $derived(!!job.model?.loading);
	let running = $derived(ACTIVE.includes(job.phase) || loading);
	let result = $derived(job.phase === 'done' && job.result ? job.result : null);

	// The training server's state as a badge.
	let serverChip = $derived.by((): [string, string] => {
		if (!overview) return ['', ''];
		if (!server) return ['not configured', 'badge-warning'];
		if (overview.health_error) return ['unreachable', 'badge-error'];
		if (!overview.health?.ok) return ['not ok', 'badge-error'];
		return serverBusy ? ['busy', 'badge-warning'] : ['ready', 'badge-success'];
	});
	let modelName = $derived([job.model?.file, job.model?.sha1?.slice(0, 7)].filter(Boolean).join(' ') || 'none');
	// Why Train is unavailable, or what the job is doing.
	let note = $derived(
		job.error ||
			job.model?.error ||
			(loading ? 'Loading the promoted model. The old one runs until it is ready.' : '') ||
			(running ? 'A job is running. Only one runs at a time.' : '') ||
			(serverBusy ? 'The training server is busy with another job.' : '')
	);
	let built = $derived(
		job.built ? `${job.built.train} train · ${job.built.val} validation · ${job.built.holdout} held out` : ''
	);

	const fmt = (v: unknown) => (typeof v === 'number' ? v.toFixed(3) : '-');
	// The change from the current model, and whether it is an improvement.
	function change(key: string, up: boolean): [string, string] {
		const c = result?.candidate?.[key];
		const r = result?.current?.[key];
		if (typeof c !== 'number' || typeof r !== 'number') return ['', ''];
		const d = c - r;
		if (Math.abs(d) < 0.0005) return ['same', 'opacity-60'];
		return [`${d > 0 ? '+' : ''}${d.toFixed(3)}`, (d > 0) === up ? 'text-success' : 'text-error'];
	}
	const total = (list: SessionRow[], key: 'ball' | 'empty' | 'clicks') =>
		list.reduce((n, s) => n + (s.error ? 0 : s[key]), 0);

	function showJob(j: Job) {
		const nowLoading = !!j.model?.loading;
		if (wasLoading && !nowLoading) load();
		wasLoading = nowLoading;
		job = j;
		if (log) requestAnimationFrame(() => log && (log.scrollTop = log.scrollHeight));
		const active = ACTIVE.includes(j.phase) || nowLoading;
		if (active && !timer) timer = setInterval(poll, 1000);
		if (!active && timer) {
			clearInterval(timer);
			timer = null;
		}
	}
	async function poll() {
		try {
			showJob(await getJson<Job>('/api/train/job'));
		} catch {
			/* the next poll retries */
		}
	}
	async function load() {
		try {
			overview = await getJson<Overview>('/api/train');
			showJob(overview.job);
		} catch (e) {
			error = (e as Error).message;
		}
	}
	async function train() {
		try {
			await sendJson('POST', '/api/train');
			error = '';
		} catch (e) {
			error = (e as Error).message;
		}
		poll();
	}
	async function promote() {
		promoting = true;
		try {
			job.promoted = (await sendJson<{ message: string }>('POST', '/api/train/promote')).message;
			error = '';
			poll();
		} catch (e) {
			error = (e as Error).message;
		}
		promoting = false;
	}

	onMount(() => {
		load();
		return () => {
			if (timer) clearInterval(timer);
		};
	});
</script>

{#snippet sessionList(list: SessionRow[], empty: string)}
	<div class="surface rounded-box overflow-hidden">
		{#if list.length}
			<table class="table">
				<thead>
					<tr><th>Session</th><th class="text-right">Ball frames</th><th class="text-right">Empty frames</th><th class="text-right">Clicks</th></tr>
				</thead>
				<tbody class="tabular-nums">
					{#each list as s (s.id)}
						<tr>
							<td>
								<span class="font-medium">Session {s.id}</span>
								{#if trained.includes(s.id)}<span class="badge badge-soft badge-success badge-sm ml-2">in the live model</span>{/if}
							</td>
							{#if s.error}
								<td class="text-error" colspan="3">{s.error}</td>
							{:else}
								<td class="text-right">{s.ball}</td><td class="text-right">{s.empty}</td><td class="text-right">{s.clicks}</td>
							{/if}
						</tr>
					{/each}
				</tbody>
				{#if list.length > 1}
					<tfoot class="tabular-nums">
						<tr><th>Total</th><th class="text-right">{total(list, 'ball')}</th><th class="text-right">{total(list, 'empty')}</th><th class="text-right">{total(list, 'clicks')}</th></tr>
					</tfoot>
				{/if}
			</table>
		{:else}
			<Empty title={empty}>Choose on the <a href="/sessions" class="link">Sessions</a> page.</Empty>
		{/if}
	</div>
{/snippet}

<h1>Train</h1>

<div class="card surface mt-4">
	<div class="card-body">
		<dl class="grid gap-4 sm:grid-cols-3">
			<div class="fact">
				<dt class="caption">Training server</dt>
				<dd class="flex flex-wrap items-center gap-2">
					{#if server}<span class="truncate">{server}</span>{/if}
					{#if serverChip[0]}<span class="badge badge-soft badge-sm {serverChip[1]}">{serverChip[0]}</span>{/if}
				</dd>
			</div>
			<div class="fact">
				<dt class="caption">Model</dt>
				<dd>
					{modelName}
					{#if job.model?.promoted}<span class="opacity-60"> · promoted {date(job.model.promoted)}</span>{/if}
				</dd>
			</div>
			<div class="fact">
				<dt class="caption">Ready to train</dt>
				<dd>{job.model?.untrained ?? 0} reviewed {job.model?.untrained === 1 ? 'session' : 'sessions'}</dd>
			</div>
		</dl>
		{#if overview && !server}
			<p class="opacity-70">Set the server under <code>[training]</code> in <code>config.toml</code>.</p>
		{/if}
		{#if overview?.health_error}
			<p class="text-error">{overview.health_error}</p>
		{/if}
		<div class="card-actions mt-2 items-center">
			<button class="btn btn-primary" disabled={!server || running || serverBusy} onclick={train}>Train</button>
			{#if running}
				<span class="badge badge-soft badge-warning">{job.phase === 'idle' ? 'loading model' : job.phase}</span>
				{#if built}<span class="opacity-70">{built}</span>{/if}
			{/if}
			{#if note}<span class={job.error || job.model?.error ? 'text-error' : 'opacity-70'}>{note}</span>{/if}
		</div>
		{#if error}<div role="alert" class="alert alert-error alert-soft">{error}</div>{/if}
	</div>
</div>

{#if job.log}
	<h2>Job log</h2>
	<div bind:this={log} class="rounded-box border-base-300 max-h-80 overflow-y-auto border bg-black p-3 font-mono text-xs whitespace-pre-wrap">{job.log}</div>
{/if}

{#if result}
	<h2>Candidate against the held out sessions</h2>
	<div class="surface rounded-box overflow-hidden">
		<table class="table">
			<thead>
				<tr><th></th><th class="text-right">Candidate</th><th class="text-right">Current</th><th class="text-right">Change</th></tr>
			</thead>
			<tbody class="tabular-nums">
				{#each SCORES as [label, key, up] (key)}
					{@const [d, tone] = change(key, up)}
					<tr>
						<td>{label}</td>
						<td class="text-right">{fmt(result.candidate?.[key])}</td>
						<td class="text-right">{fmt(result.current?.[key])}</td>
						<td class="text-right {tone}">{d}</td>
					</tr>
				{/each}
			</tbody>
		</table>
		<div class="border-base-300 flex flex-wrap items-center gap-3 border-t p-4">
			<button class="btn btn-primary" disabled={!!job.promoted || loading || promoting} onclick={promote}>Promote</button>
			<span class="text-sm opacity-70">
				{[result.epochs != null ? `${result.epochs} epochs` : '', result.frames != null ? `${result.frames} frames` : '', result.current ? '' : 'no current model to compare']
					.filter(Boolean)
					.join(' · ')}
			</span>
			{#if job.promoted}<span class="text-success text-sm">{job.promoted}</span>{/if}
		</div>
	</div>
{/if}

<h2>Training on</h2>
{@render sessionList(overview?.include ?? [], 'Nothing marked for training')}

<h2>Held out for testing</h2>
{@render sessionList(overview?.holdout ?? [], 'Nothing held out')}
