<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { getJson, sendJson } from '#lib/api.ts';
	import { live } from '#lib/socket.svelte.ts';
	import { when } from '#lib/format.ts';
	import Segmented from '#lib/components/Segmented.svelte';
	import Empty from '#lib/components/Empty.svelte';

	interface Row {
		id: string;
		clips: number;
		reviewed: number;
		trained: boolean;
		started: string | null;
		user: string | null;
		activity: string | null;
		exposure: number | null;
		gain: number | null;
		model: { file: string; sha1: string } | null;
		calibration_changed: boolean;
		tags: string[];
		notes: string;
		training: string;
	}

	const TRAINING: [string, string][] = [
		['include', 'Include'],
		['holdout', 'Hold out'],
		['none', 'None']
	];
	let sessions = $state<Row[]>([]);
	let loaded = $state(false);
	let filter = $state('');
	let newTag = $state<Record<string, string>>({});
	// The re-detection under way or last finished.
	interface Redetect {
		session: string;
		done: number;
		total: number;
		running: boolean;
		error?: string;
	}
	let redetect = $state<Redetect | null>(null);
	let redetectTimer: ReturnType<typeof setInterval> | null = null;
	// The session the delete dialog is about.
	let target = $state<Row | null>(null);
	let dialog = $state<HTMLDialogElement>();
	let busy = $state(false);

	let knownTags = $derived([...new Set(sessions.flatMap((s) => s.tags))].sort());
	let ready = $derived(
		sessions.filter((s) => s.training === 'include' && s.reviewed > 0 && !s.trained).length
	);
	let words = $derived(filter.toLowerCase().split(/\s+/).filter(Boolean));
	function matches(s: Row) {
		const text = [s.id, ...s.tags, s.user || '', s.activity || '', s.notes, s.model?.file || '']
			.join(' ')
			.toLowerCase();
		return words.every((w) => text.includes(w));
	}
	let shown = $derived(sessions.filter(matches));

	// Send a change; the reply is the session's updated row.
	async function save(s: Row, change: Partial<Row>) {
		try {
			Object.assign(s, await sendJson<Row>('PUT', `/api/sessions/${s.id}`, change));
			return true;
		} catch (e) {
			live.error = `Session ${s.id}: ${(e as Error).message}`;
			return false;
		}
	}
	async function addTag(s: Row) {
		const tag = (newTag[s.id] || '').trim();
		if (tag && !s.tags.includes(tag) && !(await save(s, { tags: [...s.tags, tag] }))) return;
		newTag[s.id] = '';
	}
	async function setNotes(s: Row, e: Event) {
		const input = e.target as HTMLInputElement;
		if (!(await save(s, { notes: input.value }))) input.value = s.notes;
	}
	// Polls the re-detection while it runs, and reports how it ended.
	async function watchRedetect() {
		try {
			const r = await getJson<Redetect | null>('/api/redetect');
			if (redetect?.running && r && !r.running) {
				if (r.error) live.error = `Re-detect session ${r.session}: ${r.error}`;
			}
			redetect = r;
		} catch {
			/* the next poll retries */
		}
		if (redetect?.running && !redetectTimer) redetectTimer = setInterval(watchRedetect, 1000);
		if (!redetect?.running && redetectTimer) {
			clearInterval(redetectTimer);
			redetectTimer = null;
		}
	}
	async function startRedetect(s: Row) {
		try {
			await sendJson('POST', `/api/sessions/${s.id}/redetect`);
			redetect = { session: s.id, done: 0, total: s.clips, running: true };
			watchRedetect();
		} catch (e) {
			live.error = (e as Error).message;
		}
	}
	function confirmDelete(s: Row) {
		target = s;
		dialog?.showModal();
	}
	async function remove() {
		if (!target) return;
		busy = true;
		try {
			await sendJson('DELETE', `/api/sessions/${target.id}`);
			sessions = sessions.filter((s) => s.id !== target?.id);
			dialog?.close();
		} catch (e) {
			live.error = (e as Error).message;
		} finally {
			busy = false;
		}
	}
	const title = (s: Row) => (s.id === 'none' ? 'Clips outside a session' : `Session ${s.id}`);
	const meta = (s: Row) => [s.user, s.activity, when(s.started)].filter(Boolean).join(' · ');

	onMount(() => {
		getJson<Row[]>('/api/sessions')
			.then((rows) => (sessions = rows))
			.catch((e) => (live.error = (e as Error).message))
			.finally(() => (loaded = true));
		watchRedetect();
	});
	onDestroy(() => {
		if (redetectTimer) clearInterval(redetectTimer);
	});
</script>

<h1>Sessions</h1>

<div class="bg-base-100/95 sticky top-14 z-10 -mx-4 flex items-center gap-3 px-4 py-3 backdrop-blur sm:mt-4">
	<input bind:value={filter} type="search" placeholder="Search sessions" class="input w-full sm:w-72" />
	<span class="text-sm whitespace-nowrap opacity-60 tabular-nums">{shown.length} of {sessions.length}</span>
	{#if ready}
		<a href="/train" class="badge badge-soft badge-success ml-auto">{ready} ready to train</a>
	{/if}
</div>
<datalist id="known-tags">
	{#each knownTags as t (t)}<option value={t}></option>{/each}
</datalist>

<div class="rows mt-2">
	{#each shown as s (s.id)}
		<details class="collapse collapse-arrow group">
			<summary class="collapse-title">
				<div class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
					<span class="font-semibold">{title(s)}</span>
					<span class="text-sm opacity-70">{meta(s)}</span>
				</div>
				<div class="mt-2 flex flex-wrap gap-1.5">
					<span class="badge badge-ghost badge-sm">{s.clips} {s.clips === 1 ? 'clip' : 'clips'}</span>
					<span class="badge badge-ghost badge-sm">{s.reviewed} reviewed</span>
					{#if s.training === 'include'}<span class="badge badge-soft badge-primary badge-sm">training</span>
					{:else if s.training === 'holdout'}<span class="badge badge-outline badge-sm">held out</span>{/if}
					{#if s.trained}<span class="badge badge-soft badge-success badge-sm">in the live model</span>{/if}
					{#if s.calibration_changed}<span class="badge badge-soft badge-warning badge-sm" title="the mount differs from the previous session's">recalibrated</span>{/if}
					{#if redetect?.running && redetect.session === s.id}
						<span class="badge badge-soft badge-warning badge-sm"><span class="loading loading-spinner loading-xs"></span>re-detecting {redetect.done} of {redetect.total}</span>
					{/if}
					{#each s.tags as t (t)}<span class="badge badge-soft badge-info badge-sm group-open:hidden">{t}</span>{/each}
				</div>
			</summary>
			<div class="collapse-content">
				<div class="flex flex-col gap-3 sm:flex-row sm:items-start">
					<div class="flex min-w-0 flex-1 flex-col gap-3">
						{#if s.model || s.exposure != null}
							<div class="flex flex-wrap gap-1.5">
								{#if s.model}<span class="badge badge-ghost badge-sm" title={s.model.sha1}>{s.model.file} {s.model.sha1.slice(0, 7)}</span>{/if}
								{#if s.exposure != null}<span class="badge badge-ghost badge-sm">{s.exposure} µs · ISO {s.gain}</span>{/if}
							</div>
						{/if}
						<div class="flex flex-wrap items-center gap-2">
							{#each s.tags as t (t)}
								<span class="badge badge-soft badge-info gap-1">
									{t}
									<button class="cursor-pointer opacity-70 hover:opacity-100" aria-label="Remove {t}" onclick={() => save(s, { tags: s.tags.filter((x) => x !== t) })}>×</button>
								</span>
							{/each}
							<input
								class="input input-sm w-36"
								placeholder="Add tag"
								list="known-tags"
								bind:value={newTag[s.id]}
								onkeydown={(e) => {
									if (e.key === 'Enter' || e.key === ',') {
										e.preventDefault();
										addTag(s);
									}
								}}
								onblur={() => addTag(s)}
							/>
							<input class="input input-sm min-w-40 flex-1" placeholder="Notes" value={s.notes} onchange={(e) => setNotes(s, e)} />
						</div>
					</div>
					<div class="flex flex-wrap items-end gap-2 sm:flex-col sm:items-end">
						<div>
							<div class="caption mb-1 sm:text-right">Training</div>
							<Segmented options={TRAINING} value={s.training} onchange={(v) => save(s, { training: v })} small />
						</div>
						<div class="flex gap-2 sm:mt-1">
							{#if s.clips > 0}
								<a href="/sessions/{s.id}/label" class="btn btn-sm">Label</a>
								<a href="/sessions/{s.id}/playback" class="btn btn-sm">Playback</a>
								<button class="btn btn-sm" disabled={!!redetect?.running} title="Detect again with the live model" onclick={() => startRedetect(s)}>Re-detect</button>
							{/if}
							<button class="btn btn-sm btn-error btn-soft" onclick={() => confirmDelete(s)}>Delete</button>
						</div>
					</div>
				</div>
			</div>
		</details>
	{:else}
		<div>
			{#if !loaded}
				<Empty title="Loading" />
			{:else if sessions.length}
				<Empty title="Nothing matches">Try fewer words.</Empty>
			{:else}
				<Empty title="No sessions yet">Start one on the <a href="/" class="link">Tracker</a> page.</Empty>
			{/if}
		</div>
	{/each}
</div>

<dialog bind:this={dialog} class="modal">
	<div class="modal-box">
		{#if target}
			<h3 class="text-lg font-semibold">Delete {title(target)}?</h3>
			<p class="mt-2 opacity-70">
				{#if target.clips}
					Its {target.clips} {target.clips === 1 ? 'clip' : 'clips'} and their labels go with it.
				{:else}
					It has no clips.
				{/if}
			</p>
			<div class="modal-action">
				<form method="dialog"><button class="btn" disabled={busy}>Cancel</button></form>
				<button class="btn btn-error" disabled={busy} onclick={remove}>Delete session</button>
			</div>
		{/if}
	</div>
	<form method="dialog" class="modal-backdrop"><button>close</button></form>
</dialog>
