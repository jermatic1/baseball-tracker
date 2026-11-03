<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { getJson, sendJson } from '#lib/api.ts';
	import ClipList from '#lib/components/ClipList.svelte';
	import Segmented from '#lib/components/Segmented.svelte';
	import Empty from '#lib/components/Empty.svelte';

	interface Label {
		clip: string;
		index: number;
		kind: string;
		cx: number;
		cy: number;
		filled?: boolean;
	}
	interface Point {
		index: number;
		cx: number;
		cy: number;
	}
	interface Track {
		kind: string;
		start: number;
		end: number;
		points: Point[];
		conf: number;
		verdict?: string;
	}

	const base = `/sessions/${page.params.id}/label/api`;
	const THUMB = 64;
	const CROP = 56;

	let clips = $state<string[]>([]);
	let clipId = $state<string | null>(null);
	let n = $state(0);
	let i = $state(0);
	let labels = $state<Label[]>([]);
	let emptyClips = $state(new Set<string>());
	let bitmap: ImageBitmap | null = null;
	let mode = $state<'tracks' | 'frames'>('tracks');
	let tracks = $state<Track[]>([]);
	let sel = $state(0);
	let thumbs = new Map<number, Promise<ImageBitmap>>();
	let box = $state(32);
	let step = $state(2);
	let status = $state('');
	let canvas = $state<HTMLCanvasElement>();
	let zoom = $state<HTMLCanvasElement>();
	let zoomSide = $state<'left' | 'right'>('right');
	let zoomShown = $state(false);
	let tracksEl = $state<HTMLUListElement>();
	let showKeys = $state(false);
	const session = page.params.id === 'local' ? '' : page.params.id;

	let balls = $derived(labels.filter((b) => b.kind === 'ball' && !b.filled).length);
	let empties = $derived(labels.filter((b) => b.kind === 'empty').length);
	let clipEmpty = $derived(clipId != null && emptyClips.has(clipId));

	const boxesFor = (index: number) => labels.filter((b) => b.clip === clipId && b.index === index);

	function draw() {
		if (!bitmap || !canvas) return;
		const ctx = canvas.getContext('2d')!;
		canvas.width = bitmap.width;
		canvas.height = bitmap.height;
		ctx.drawImage(bitmap, 0, 0);
		const size = Number(box) || 32;
		for (const b of boxesFor(i)) {
			ctx.strokeStyle = b.kind === 'ball' ? '#0f0' : '#f66';
			ctx.lineWidth = b.filled ? 1 : 2;
			if (b.kind === 'ball') {
				ctx.strokeRect(b.cx - size / 2, b.cy - size / 2, size, size);
			} else {
				ctx.strokeRect(8, 8, 40, 16);
				ctx.fillStyle = '#f66';
				ctx.fillText('empty', 12, 20);
			}
		}
	}
	async function show(index: number) {
		if (!clipId || n === 0) return;
		i = Math.max(0, Math.min(n - 1, index));
		const r = await fetch(`${base}/clips/${clipId}/frame/${i}`, { cache: 'no-store' });
		if (!r.ok) return;
		const blob = await r.blob();
		if (bitmap) bitmap.close();
		bitmap = await createImageBitmap(blob);
		draw();
	}
	async function loadLabels() {
		const body = await getJson<{ labels?: Label[]; empty_clips?: string[]; box?: number }>(`${base}/labels`);
		labels = body.labels || [];
		emptyClips = new Set(body.empty_clips || []);
		if (body.box) box = body.box;
		draw();
	}
	async function save(kind: string, cx: number | null, cy: number | null) {
		try {
			await sendJson('POST', `${base}/labels`, { clip: clipId, index: i, kind, cx, cy, box: Number(box) || 32 });
		} catch (e) {
			status = (e as Error).message;
			return;
		}
		await loadLabels();
		await show(i + (Number(step) || 1));
	}
	async function toggleEmptyClip() {
		if (!clipId) return;
		const empty = !emptyClips.has(clipId);
		try {
			await sendJson('POST', `${base}/empty_clips`, { clip: clipId, empty });
		} catch (e) {
			status = (e as Error).message;
			return;
		}
		await loadLabels();
		if (empty) stepClip(1);
	}
	function stepClip(d: number) {
		const at = clips.indexOf(clipId ?? '');
		const next = clips[at + d];
		if (next) selectClip(next);
	}
	function frameBitmap(index: number) {
		if (!thumbs.has(index)) {
			thumbs.set(
				index,
				fetch(`${base}/clips/${clipId}/frame/${index}`)
					.then((r) => (r.ok ? r.blob() : Promise.reject(r.status)))
					.then((b) => createImageBitmap(b))
			);
		}
		return thumbs.get(index)!;
	}
	function samplePoints(points: Point[]) {
		const k = Math.min(5, points.length);
		if (k === 1) return [points[0]];
		return Array.from({ length: k }, (_, j) => points[Math.round((j * (points.length - 1)) / (k - 1))]);
	}
	// Draws a track's sampled frames, cropped around the ball, across a canvas.
	function strip(el: HTMLCanvasElement, t: Track) {
		const id = clipId;
		el.width = THUMB * Math.min(5, t.points.length);
		el.height = THUMB;
		const cx = el.getContext('2d')!;
		samplePoints(t.points).forEach((p, j) => {
			frameBitmap(p.index)
				.then((bmp) => {
					if (id !== clipId) return;
					cx.drawImage(bmp, p.cx - CROP / 2, p.cy - CROP / 2, CROP, CROP, j * THUMB, 0, THUMB, THUMB);
					cx.strokeStyle = '#0f0';
					cx.strokeRect(j * THUMB + THUMB / 2 - 9, THUMB / 2 - 9, 18, 18);
				})
				.catch(() => {});
		});
	}
	function selectTrack(k: number) {
		if (!tracks.length) return;
		sel = Math.max(0, Math.min(tracks.length - 1, k));
		(tracksEl?.children[sel] as HTMLElement | undefined)?.scrollIntoView({ block: 'nearest' });
	}
	async function loadTracks() {
		const id = clipId;
		const body = await getJson<{ tracks?: Track[] }>(`${base}/clips/${id}/tracks`);
		if (id !== clipId) return;
		tracks = body.tracks || [];
		sel = Math.max(0, tracks.findIndex((t) => !t.verdict));
		selectTrack(sel);
	}
	async function review(list: Track[], verdict: string) {
		for (const t of list) {
			try {
				await sendJson('POST', `${base}/tracks/review`, { clip: clipId, verdict, points: t.points });
			} catch (e) {
				status = (e as Error).message;
				return;
			}
			t.verdict = verdict;
		}
		await loadLabels();
		if (tracks.length && tracks.every((t) => t.verdict)) {
			stepClip(1);
			return;
		}
		const next = tracks.findIndex((t, k) => k > sel && !t.verdict);
		selectTrack(next >= 0 ? next : sel);
	}
	function setMode(m: 'tracks' | 'frames') {
		mode = m;
		if (m === 'frames') show(tracks[sel] ? tracks[sel].start : i);
		else draw();
	}
	function imagePoint(ev: MouseEvent) {
		const r = canvas!.getBoundingClientRect();
		return {
			x: ((ev.clientX - r.left) * canvas!.width) / r.width,
			y: ((ev.clientY - r.top) * canvas!.height) / r.height
		};
	}
	function magnify(ev: MouseEvent) {
		if (!bitmap || !zoom) return;
		const p = imagePoint(ev);
		const r = canvas!.getBoundingClientRect();
		const nearCorner = ev.clientY - r.top < 220;
		if (nearCorner && zoomSide === 'right' && r.right - ev.clientX < 220) zoomSide = 'left';
		if (nearCorner && zoomSide === 'left' && ev.clientX - r.left < 220) zoomSide = 'right';
		zoomShown = true;
		const s = 22;
		const zctx = zoom.getContext('2d')!;
		zctx.imageSmoothingEnabled = false;
		zctx.drawImage(bitmap, p.x - s, p.y - s, s * 2, s * 2, 0, 0, 180, 180);
		zctx.strokeStyle = '#0f0';
		zctx.strokeRect(78, 78, 24, 24);
	}
	function keydown(ev: KeyboardEvent) {
		const target = ev.target as HTMLInputElement;
		if (target.type === 'number' || ev.ctrlKey || ev.metaKey || ev.altKey) return;
		if (ev.key === 'e') toggleEmptyClip();
		if (ev.key === 't') setMode(mode === 'tracks' ? 'frames' : 'tracks');
		if (mode === 'tracks') {
			const t = tracks[sel];
			if (ev.key === 'j') selectTrack(sel + 1);
			if (ev.key === 'k') selectTrack(sel - 1);
			if (ev.key === 'n') stepClip(1);
			if (ev.key === 'p') stepClip(-1);
			if (ev.key === 'a' && t) review([t], 'ball');
			if (ev.key === 'r' && t) review([t], 'reject');
			if (ev.key === 'A') acceptMoving();
			return;
		}
		if (ev.key === 'n') save('empty', null, null);
		if (ev.key === 'u') save('clear', null, null);
		if (ev.key === 'ArrowLeft') show(i - 1);
		if (ev.key === 'ArrowRight') show(i + 1);
	}
	// Key, then what it does, for the shortcut reference.
	const KEYS: [string, string][] = [
		['a', 'Accept the track as a ball'],
		['r', 'Reject the track'],
		['A', 'Accept every moving track in the clip'],
		['j / k', 'Next or previous track'],
		['n / p', 'Next or previous clip'],
		['e', 'No ball anywhere in this clip, then the next clip'],
		['t', 'Switch between tracks and frames'],
		['← / →', 'Previous or next frame'],
		['n', 'No ball in this frame'],
		['u', 'Undo this frame']
	];
	const acceptMoving = () =>
		review(
			tracks.filter((x) => x.kind === 'moving' && x.verdict !== 'ball'),
			'ball'
		);
	async function selectClip(id: string) {
		clipId = id;
		thumbs = new Map();
		tracks = [];
		const clip = await getJson<{ meta?: { frames: number } }>(`${base}/clips/${id}`);
		n = clip.meta?.frames || 0;
		await loadTracks();
		await show(mode === 'frames' && tracks[sel] ? tracks[sel].start : 0);
	}

	onMount(() => {
		(async () => {
			try {
				const session = await getJson<{ clips?: string[] }>(`${base}/session`);
				await loadLabels();
				clips = session.clips || [];
				if (clips.length) selectClip(clips[0]);
			} catch (e) {
				status = (e as Error).message;
			}
		})();
		return () => bitmap?.close();
	});
</script>

<svelte:window onkeydown={keydown} />

<div class="flex flex-wrap items-baseline gap-3">
	<h1>Label</h1>
	{#if session}<span class="badge badge-ghost">Session {session}</span>{/if}
	<span class="flex basis-full gap-1.5 sm:ml-auto sm:basis-auto">
		<span class="badge badge-ghost badge-sm">{balls} ball clicks</span>
		<span class="badge badge-ghost badge-sm">{empties} empty frames</span>
		<span class="badge badge-ghost badge-sm">{emptyClips.size} empty clips</span>
	</span>
</div>

<div class="mt-4 grid gap-4 md:grid-cols-[10rem_1fr]">
	<ClipList {clips} current={clipId} marked={emptyClips} onselect={selectClip} />

	<section class="min-w-0">
		<div class="flex flex-wrap items-center gap-2">
			<Segmented options={[['tracks', 'Tracks'], ['frames', 'Frames']]} value={mode} onchange={(m) => setMode(m as 'tracks' | 'frames')} />
			<button class="btn {clipEmpty ? 'btn-error btn-soft' : ''}" aria-pressed={clipEmpty} onclick={toggleEmptyClip}>
				{clipEmpty ? 'Marked empty' : 'No ball in clip'}
			</button>
			<span class="ml-auto flex gap-2">
				<div class="join">
					<button class="btn btn-square join-item" aria-label="previous clip" onclick={() => stepClip(-1)}>‹</button>
					<button class="btn btn-square join-item" aria-label="next clip" onclick={() => stepClip(1)}>›</button>
				</div>
				<button class="btn {showKeys ? 'btn-selected' : ''}" aria-pressed={showKeys} onclick={() => (showKeys = !showKeys)}>Shortcuts</button>
			</span>
		</div>
		{#if showKeys}
			<dl class="card surface mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 p-4 text-sm">
				{#each KEYS as [key, what] (key + what)}
					<dt><kbd class="kbd kbd-sm">{key}</kbd></dt><dd class="opacity-70">{what}</dd>
				{/each}
			</dl>
		{/if}
		{#if status}<div role="alert" class="alert alert-error alert-soft mt-3">{status}</div>{/if}

		{#if mode === 'tracks'}
			<ul class="list surface rounded-box mt-3" bind:this={tracksEl}>
				{#each tracks as t, k (`${clipId}:${k}`)}
					<li
						role="option"
						aria-selected={k === sel}
						tabindex="-1"
						class="list-row flex flex-wrap items-center gap-3 {k === sel ? 'bg-base-300/60' : ''} {t.verdict ? 'opacity-50' : ''}"
						onclick={(ev) => (ev.target as HTMLElement).tagName !== 'BUTTON' && selectTrack(k)}
						onkeydown={() => {}}
					>
						<canvas use:strip={t} class="rounded-field max-w-full bg-black"></canvas>
						<div class="min-w-40 flex-1 text-sm">
							<div class="flex flex-wrap items-center gap-1.5">
								<span class="badge badge-sm {t.kind === 'moving' ? 'badge-soft badge-success' : t.kind === 'resting' ? 'badge-soft badge-warning' : 'badge-ghost'}">{t.kind}</span>
								{#if t.verdict}<span class="badge badge-sm badge-soft {t.verdict === 'ball' ? 'badge-success' : 'badge-error'}">{t.verdict === 'ball' ? 'ball' : 'rejected'}</span>{/if}
							</div>
							<div class="mt-1 opacity-70 tabular-nums">Frames {t.start}–{t.end} · {t.points.length} points · {Math.round(t.conf * 100)}% confidence</div>
						</div>
						<div class="flex gap-2">
							<button class="btn btn-primary btn-sm" onclick={() => { sel = k; review([t], 'ball'); }}>Ball</button>
							<button class="btn btn-sm" onclick={() => { sel = k; review([t], 'reject'); }}>Reject</button>
						</div>
					</li>
				{:else}
					<li><Empty title="No detections in this clip">Mark it empty, or open Frames and click the ball.</Empty></li>
				{/each}
				{#if tracks.some((t) => t.kind === 'moving' && t.verdict !== 'ball')}
					<li class="list-row"><button class="btn btn-sm" onclick={acceptMoving}>Accept every moving track</button></li>
				{/if}
			</ul>
		{:else}
			<div class="mt-3 flex items-center gap-3">
				<div class="join">
					<button class="btn btn-square join-item" aria-label="previous frame" onclick={() => show(i - 1)}>‹</button>
					<button class="btn btn-square join-item" aria-label="next frame" onclick={() => show(i + 1)}>›</button>
				</div>
				<input type="range" min="0" max={Math.max(0, n - 1)} bind:value={i} oninput={() => show(Number(i))} class="range range-sm min-w-0 flex-1" />
				<span class="text-sm whitespace-nowrap tabular-nums">{i} / {Math.max(0, n - 1)}</span>
			</div>
			<div class="mt-2 flex flex-wrap items-center gap-2">
				<button class="btn" onclick={() => save('empty', null, null)}>No ball here</button>
				<button class="btn" onclick={() => save('clear', null, null)}>Undo frame</button>
				<label class="ml-auto flex items-center gap-2 text-sm opacity-80">Box <input type="number" min="8" max="80" bind:value={box} onchange={draw} class="input input-sm w-18" /></label>
				<label class="flex items-center gap-2 text-sm opacity-80">Step <input type="number" min="1" max="8" bind:value={step} class="input input-sm w-16" /></label>
			</div>
			<div class="relative mt-3">
				<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
				<canvas
					bind:this={canvas}
					class="rounded-box block w-full cursor-crosshair bg-black"
					onclick={(ev) => { const p = imagePoint(ev); save('ball', p.x, p.y); }}
					onmousemove={magnify}
					onmouseleave={() => (zoomShown = false)}
				></canvas>
				<canvas
					bind:this={zoom}
					width="180"
					height="180"
					class="rounded-field border-base-300 pointer-events-none absolute top-3 hidden border bg-black {zoomSide === 'right' ? 'right-3' : 'left-3'} {zoomShown ? 'pointer-fine:block' : ''}"
				></canvas>
			</div>
			<p class="mt-3 text-sm opacity-70">Click the ball. Gaps of up to 12 frames fill in a straight line.</p>
		{/if}
	</section>
</div>
