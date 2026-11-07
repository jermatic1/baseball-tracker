<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { getJson } from '#lib/api.ts';
	import ClipList from '#lib/components/ClipList.svelte';
	import Empty from '#lib/components/Empty.svelte';
	import ZonePlot from '#lib/components/ZonePlot.svelte';
	import type { Location, Pitch } from '#lib/socket.svelte.ts';

	interface Ball {
		x: number;
		y: number;
		w: number;
		h: number;
		conf?: number;
		depth_m?: number | null;
	}
	interface Hit {
		kind: string;
		hit_type?: string;
		segment: number;
		frame_start: number;
		frame_end: number;
		exit_velocity: number;
		pitch_velocity?: number | null;
		launch_angle: number;
		spray_angle: number;
		contact?: { kind: string; x: number; y: number; z: number };
		hit?: { carry: number; hang_time: number };
		pitch?: Pitch;
		samples: number;
		confident: boolean;
	}
	interface Clip {
		meta?: { frames: number };
		detections?: { frames?: { index: number; balls?: Ball[] }[] };
		events?: Hit[];
	}

	const base = `/sessions/${page.params.id}/playback/api`;
	let clips = $state<string[]>([]);
	let clipId = $state<string | null>(null);
	let clip = $state<Clip | null>(null);
	let n = $state(0);
	let i = $state(0);
	let playing = $state(false);
	let fps = 30;
	let status = $state('');
	let canvas: HTMLCanvasElement;

	function ballsFor(index: number) {
		const frames = clip?.detections?.frames || [];
		const row = frames.find((f) => f.index === index) || frames[index];
		return row?.balls || [];
	}
	function draw(bitmap: ImageBitmap, index: number) {
		const ctx = canvas.getContext('2d')!;
		const h = bitmap.height;
		canvas.width = bitmap.width;
		canvas.height = h;
		ctx.drawImage(bitmap, 0, 0);
		for (const b of ballsFor(index)) {
			const ground = b.y + b.h > h * 0.8;
			ctx.strokeStyle = ground ? '#8cf' : '#0f0';
			ctx.lineWidth = ground ? 1 : 2;
			ctx.strokeRect(b.x, b.y, b.w, b.h);
			const depth = b.depth_m == null ? '' : ' ' + Number(b.depth_m).toFixed(2) + 'm';
			ctx.fillStyle = '#fff';
			ctx.fillText((b.conf || 0).toFixed(2) + depth, b.x, Math.max(12, b.y - 2));
		}
	}
	async function show(index: number) {
		if (!clipId || n === 0) return;
		i = Math.max(0, Math.min(n - 1, index));
		const r = await fetch(`${base}/clips/${clipId}/frame/${i}`, { cache: 'no-store' });
		if (!r.ok) return;
		const bmp = await createImageBitmap(await r.blob());
		draw(bmp, i);
		bmp.close();
	}
	const loc = (l: Location) => `${l.side.toFixed(0)} in, ${l.height.toFixed(0)} in high`;
	let pitches = $derived((clip?.events ?? []).flatMap((h) => (h.pitch ? [h.pitch] : [])));
	const session = page.params.id === 'local' ? '' : page.params.id;
	// Label and value pairs for one event's card.
	function facts(h: Hit): [string, string][] {
		const out: [string, string][] = [
			['Frames', `${h.frame_start}–${h.frame_end}`],
			['Launch', `${h.launch_angle.toFixed(1)}°`],
			['Spray', `${h.spray_angle.toFixed(1)}°`]
		];
		if (h.pitch_velocity != null) out.push(['Pitch', `${h.pitch_velocity.toFixed(1)} mph`]);
		if (h.hit) out.push(['Carry', `${h.hit.carry.toFixed(0)} ft`], ['Hang', `${h.hit.hang_time.toFixed(1)} s`]);
		if (h.contact) out.push(['Contact', `${h.contact.kind} at ${h.contact.x.toFixed(1)}, ${h.contact.y.toFixed(1)}, ${h.contact.z.toFixed(1)} ft`]);
		if (h.pitch) {
			out.push(['Release', `${h.pitch.release_velocity.toFixed(1)} mph`], ['Plate', `${h.pitch.plate_velocity.toFixed(1)} mph`]);
			out.push(['Location', loc(h.pitch.location)], ['Simulated', loc(h.pitch.simulated_location)]);
		}
		out.push(['Samples', String(h.samples)]);
		return out;
	}
	async function selectClip(id: string) {
		playing = false;
		clipId = id;
		const c = await getJson<Clip>(`${base}/clips/${id}`);
		clip = c;
		n = c.meta?.frames || 0;
		await show(0);
	}
	async function togglePlay() {
		playing = !playing;
		while (playing) {
			if (i >= n - 1) {
				playing = false;
				break;
			}
			await show(i + 1);
			await new Promise((r) => setTimeout(r, 1000 / fps));
		}
	}

	onMount(() => {
		(async () => {
			try {
				const session = await getJson<{ clips?: string[]; config?: { capture?: { fps?: number } } }>(`${base}/session`);
				fps = session.config?.capture?.fps || 30;
				clips = session.clips || [];
				if (clips.length) await selectClip(clips[0]);
			} catch (e) {
				status = (e as Error).message;
			}
		})();
		return () => {
			playing = false;
		};
	});
</script>

<div class="flex flex-wrap items-baseline gap-3">
	<h1>Playback</h1>
	{#if session}<span class="badge badge-ghost">Session {session}</span>{/if}
</div>

<div class="mt-4 grid gap-4 md:grid-cols-[10rem_1fr]">
	<ClipList {clips} current={clipId} onselect={selectClip} />

	<section class="min-w-0">
		<canvas bind:this={canvas} class="rounded-box w-full bg-black"></canvas>
		<div class="mt-3 flex items-center gap-3">
			<button class="btn w-24" onclick={togglePlay}>{playing ? 'Pause' : 'Play'}</button>
			<input type="range" min="0" max={Math.max(0, n - 1)} bind:value={i} oninput={() => { playing = false; show(Number(i)); }} class="range range-sm min-w-0 flex-1" />
			<span class="text-sm whitespace-nowrap tabular-nums">{i} / {Math.max(0, n - 1)}</span>
		</div>
		{#if status}<div role="alert" class="alert alert-error alert-soft mt-3">{status}</div>{/if}
		{#if pitches.length}
			<div class="card surface mt-4">
				<div class="card-body gap-4 p-3 sm:flex-row sm:items-start sm:p-4">
					<div class="mx-auto w-48 shrink-0 sm:mx-0"><ZonePlot zone={pitches[0].zone} {pitches} /></div>
					<div class="min-w-0 flex-1 text-sm">
						<div class="caption mb-1">Strike zone</div>
						<p class="opacity-70">From the mound. Hollow is where the ball crossed the plate; filled is the same throw from the simulated distance, green for a strike and red for a ball.</p>
					</div>
				</div>
			</div>
		{/if}

		<ul class="rows mt-4">
			{#each clip?.events ?? [] as h, k (k)}
				<li class="p-4">
					<div class="flex items-center justify-between text-xs opacity-60">
						<span>{h.kind}{h.hit_type ? ` · ${h.hit_type.replace('_', ' ')}` : ''}</span>
						<span class="badge badge-sm badge-soft {h.confident ? 'badge-success' : 'badge-warning'}">{h.confident ? 'confident' : 'low confidence'}</span>
					</div>
					<div class="mt-1 flex items-baseline gap-2">
						<span class="text-2xl font-semibold tabular-nums">{h.exit_velocity.toFixed(1)}</span>
						<span class="text-sm opacity-70">mph</span>
					</div>
					<dl class="mt-2 grid grid-cols-3 gap-x-3 gap-y-1.5 sm:grid-cols-4">
						{#each facts(h) as [k, v] (k)}
							<div class="fact min-w-0"><dt class="caption">{k}</dt><dd class="truncate">{v}</dd></div>
						{/each}
					</dl>
				</li>
			{:else}
				<li><Empty title="No events">Nothing was measured in this clip.</Empty></li>
			{/each}
		</ul>
	</section>
</div>
