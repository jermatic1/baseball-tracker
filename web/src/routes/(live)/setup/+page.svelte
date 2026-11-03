<script lang="ts">
	import { onMount } from 'svelte';
	import { live, on, send, type Message, type Status } from '#lib/socket.svelte.ts';
	import Stepper from '#lib/components/Stepper.svelte';

	let img: HTMLImageElement;
	let boxes: HTMLCanvasElement;
	let overlay: HTMLCanvasElement;
	let view: HTMLDivElement;
	let url: string | null = null;
	let controls = { exposure: 600, gain: 1600, ir_flood: 0, ir_dot: 0 };

	// Calibration: corners in frame pixels, whether the user has edited them,
	// and the fit currently shown for them.
	let hasFrame = $state(false);
	let calibrating = $state(false);
	let dots: number[][] = [];
	let touched = $state(false);
	// Clear puts the page in manual mode: taps place corners instead of
	// picking a plate.
	let manual = $state(false);
	let shown: Message = null;
	let dragging = -1;
	let sendTimer: ReturnType<typeof setTimeout> | undefined;
	interface Mount {
		distance: number;
		height: number;
		lateral: number;
		yaw?: number;
		pitch?: number;
	}
	let mount = $state<Mount | null>(null);
	// The plate fit while calibrating, once there is one.
	let plateFit = $state<{ rms: number; source: string; applied: boolean } | null>(null);
	let warnings = $state<string[]>([]);
	let canApply = $state(false);
	let hasDots = $state(false);
	let dotCount = $state(0);

	// Each stepper's setting, label, step, and how to show its value.
	const steppers: [keyof typeof controls, string, number, (v: number) => string][] = [
		['exposure', 'Exposure', 100, (v) => `${v} µs`],
		['gain', 'ISO', 100, (v) => String(v)],
		['ir_flood', 'IR flood', 0.2, (v) => `${Math.round(v * 100)}%`],
		['ir_dot', 'IR dot', 0.2, (v) => `${Math.round(v * 100)}%`]
	];
	let settings = $derived(
		live.status
			? { exposure: live.status.exposure, gain: live.status.gain, ir_flood: live.status.ir_flood, ir_dot: live.status.ir_dot }
			: null
	);
	// The camera's rates and counts as label and value pairs.
	let cameraStats = $derived.by((): [string, string][] => {
		const c = live.status?.camera;
		if (!c) return [];
		return [
			['left', `${c.left_rate} fps`],
			['right', `${c.right_rate} fps`],
			['pairs', `${c.pair_rate} fps`],
			['gaps', `L${c.left_gaps} R${c.right_gaps}`],
			['unpaired', String(c.unpaired)]
		];
	});
	let chips = $derived.by((): [string, string][] => {
		const m = live.status;
		if (!m) return [];
		const out: [string, string][] = [[m.capturing ? 'capturing' : 'idle', m.capturing ? 'badge-soft badge-success' : '']];
		if (m.recording) out.push(['recording', 'badge-soft badge-error']);
		const d = m.detector;
		if (d) {
			out.push([`detector ${d.state} · ${d.rate.toFixed(0)} fps on ${d.provider}`, '']);
			if (d.lag || d.drops) out.push([`${d.lag} behind, ${d.drops} dropped`, 'badge-soft badge-warning']);
			out.push([`${d.clips} clips`, '']);
			if (d.last_event) out.push([d.last_event, '']);
		}
		const others = m.clients.filter((c) => c !== 'page' && !c.endsWith(' page'));
		if (others.length) out.push([`simulators ${others.join(', ')}`, '']);
		return out;
	});

	function status(m: Status) {
		controls = { exposure: m.exposure, gain: m.gain, ir_flood: m.ir_flood, ir_dot: m.ir_dot };
		drawBoxes(m.detector ? m.detector.boxes : []);
		setCalibrating(m.calibrating);
		if (!m.calibrating) mount = m.mount;
	}
	function calibration(m: Message) {
		if (m.source === 'auto' && !m.applied) {
			if (touched) return;
			dots = m.found ? m.corners.map((p: number[]) => [p[0], p[1]]) : [];
		} else if (m.source === 'picked' && !m.applied) {
			dots = m.found ? m.corners.map((p: number[]) => [p[0], p[1]]) : [];
			touched = false;
			manual = false;
		} else if (m.source === 'clicked' && !m.applied && dots.length < 5) {
			return;
		}
		shown = m;
		showCalibration();
	}

	function fit(canvas: HTMLCanvasElement) {
		canvas.width = img.naturalWidth || 640;
		canvas.height = img.naturalHeight || 400;
		canvas.style.width = img.clientWidth + 'px';
		canvas.style.height = img.clientHeight + 'px';
		return canvas.getContext('2d')!;
	}
	function drawBoxes(list: number[][]) {
		const ctx = fit(boxes);
		ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
		ctx.strokeStyle = '#f80';
		ctx.lineWidth = 2;
		list.forEach((b) => ctx.strokeRect(b[0], b[1], b[2], b[3]));
	}
	function showCalibration() {
		const m = shown && shown.found ? shown : null;
		drawPlate(m);
		canApply = !!m;
		hasDots = dots.length > 0;
		dotCount = dots.length;
		warnings = m ? m.warnings : [];
		plateFit = m ? { rms: m.rms, source: m.source, applied: m.applied } : null;
		if (m) mount = m.mount;
	}
	function drawPlate(m: Message) {
		const ctx = fit(overlay);
		ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
		if (!calibrating) return;
		const poly = (pts: number[][], color: string, closed: boolean) => {
			if (!pts || pts.length < 2) return;
			ctx.strokeStyle = color;
			ctx.lineWidth = 2;
			ctx.beginPath();
			pts.forEach((p, i) => (i ? ctx.lineTo(p[0], p[1]) : ctx.moveTo(p[0], p[1])));
			if (closed) ctx.closePath();
			ctx.stroke();
		};
		if (m) poly(m.reprojected, '#ff0', true);
		poly(dots, '#0f0', dots.length === 5);
		ctx.font = 'bold 12px sans-serif';
		ctx.textAlign = 'center';
		ctx.textBaseline = 'middle';
		dots.forEach((p, i) => {
			ctx.fillStyle = '#0f0';
			ctx.beginPath();
			ctx.arc(p[0], p[1], 7, 0, 2 * Math.PI);
			ctx.fill();
			ctx.fillStyle = '#000';
			ctx.fillText(String(i + 1), p[0], p[1]);
		});
	}
	function setCalibrating(on: boolean) {
		if (on === calibrating) return;
		calibrating = on;
		dots = [];
		touched = false;
		manual = false;
		shown = null;
		dragging = -1;
		dotCount = 0;
		clearTimeout(sendTimer);
		warnings = [];
		plateFit = null;
		if (on) showCalibration();
		else drawPlate(null);
	}
	// A user edit: forget the shown fit and, with all five corners, ask for a
	// new one once the edits pause.
	function editedDots() {
		touched = true;
		shown = null;
		clearTimeout(sendTimer);
		if (dots.length === 5) {
			sendTimer = setTimeout(
				() => send({ type: 'calibrate', action: 'corners', corners: dots }),
				150
			);
		}
		showCalibration();
	}
	function framePoint(e: PointerEvent) {
		const r = img.getBoundingClientRect();
		const sx = (img.naturalWidth || 640) / r.width;
		const sy = (img.naturalHeight || 400) / r.height;
		return { x: (e.clientX - r.left) * sx, y: (e.clientY - r.top) * sy, reach: 10 * sx };
	}
	function pointerDown(e: PointerEvent) {
		if (!calibrating) return;
		const p = framePoint(e);
		dragging = dots.findIndex((d) => Math.hypot(d[0] - p.x, d[1] - p.y) <= p.reach);
		if (dragging < 0) {
			if (manual) {
				if (dots.length < 5) {
					dots.push([p.x, p.y]);
					editedDots();
				}
			} else {
				send({ type: 'calibrate', action: 'pick', point: [p.x, p.y] });
			}
		}
		if (dragging >= 0) view.setPointerCapture(e.pointerId);
		e.preventDefault();
	}
	function pointerMove(e: PointerEvent) {
		if (dragging < 0) return;
		const p = framePoint(e);
		dots[dragging] = [p.x, p.y];
		editedDots();
	}
	function undo() {
		if (!calibrating || !dots.length) return;
		dots.pop();
		editedDots();
	}
	function clear() {
		dots = [];
		manual = true;
		editedDots();
	}
	function step(key: keyof typeof controls, delta: number) {
		const value = controls[key] + delta;
		send({ type: 'set', [key]: key === 'exposure' || key === 'gain' ? Math.round(value) : value });
	}
	function keydown(e: KeyboardEvent) {
		if (e.key === 'u' && !e.ctrlKey && !e.metaKey && !e.altKey) undo();
	}

	onMount(() => {
		const preview = () => send({ type: 'preview', enabled: true });
		if (live.connected) preview();
		if (live.status) status(live.status);
		const offs = [
			on('open', preview),
			on('blob', (data: Blob) => {
				const next = URL.createObjectURL(data);
				img.src = next;
				hasFrame = true;
				if (url) URL.revokeObjectURL(url);
				url = next;
			}),
			on('status', status),
			on('calibration', calibration)
		];
		return () => {
			offs.forEach((off) => off());
			send({ type: 'preview', enabled: false });
			clearTimeout(sendTimer);
			if (url) URL.revokeObjectURL(url);
		};
	});
</script>

<svelte:window onkeydown={keydown} />

<h1>Setup</h1>

<div class="mt-4 grid gap-4 lg:grid-cols-3">
	<div class="card surface lg:col-span-2">
		<div class="card-body p-2 sm:p-3">
			<div
				bind:this={view}
				role="application"
				aria-label="camera preview"
				class="relative w-full touch-none select-none"
				onpointerdown={pointerDown}
				onpointermove={pointerMove}
				onpointerup={() => (dragging = -1)}
			>
				<img bind:this={img} alt="" draggable="false" class="rounded-field block aspect-[8/5] w-full bg-black" />
				<canvas bind:this={boxes} class="pointer-events-none absolute top-0 left-0"></canvas>
				<canvas bind:this={overlay} class="pointer-events-none absolute top-0 left-0"></canvas>
				{#if !hasFrame}
					<div class="pointer-events-none absolute inset-0 flex items-center justify-center text-sm opacity-60">
						{live.status?.camera ? 'Waiting for a frame' : 'Waiting for the camera'}
					</div>
				{/if}
			</div>
			<div class="mt-1 flex flex-wrap items-center gap-1.5 px-1">
				{#each chips as [text, tone] (text)}<span class="badge badge-sm {tone || 'badge-ghost'}">{text}</span>{/each}
				<span class="ml-auto flex gap-2">
					<button class="btn btn-sm" disabled={!live.status || live.status.recording} onclick={() => send({ type: 'record', action: 'start' })}>Record</button>
					<button class="btn btn-sm" disabled={!live.status?.recording} onclick={() => send({ type: 'record', action: 'stop' })}>Stop</button>
				</span>
			</div>
			{#if cameraStats.length}
				<dl class="mt-2 grid grid-cols-3 gap-2 px-1 sm:grid-cols-5">
					{#each cameraStats as [k, v] (k)}
						<div class="fact"><dt class="caption">{k}</dt><dd>{v}</dd></div>
					{/each}
				</dl>
			{/if}
		</div>
	</div>

	<div class="space-y-4">
		<div class="card surface">
			<div class="card-body">
				{#if calibrating}
					<div class="flex items-center gap-3">
						<div class="min-w-0 flex-1">
							<div class="caption">Calibrating</div>
							<div class="text-lg font-semibold">{dotCount} of 5 corners</div>
						</div>
						<span class="badge badge-soft badge-warning badge-sm">capture paused</span>
					</div>
					{#if plateFit}
						<p class="opacity-70">
							{plateFit.applied ? 'Applied' : 'Fitted'} from {plateFit.source === 'auto' ? 'the detected plate' : plateFit.source === 'picked' ? 'the plate you tapped' : 'your corners'} · {plateFit.rms.toFixed(2)} px error
						</p>
					{:else if !touched}
						<p class="opacity-70">Plate not found. Tap the white of the plate.</p>
					{/if}
					{#each warnings as w (w)}
						<p class="text-error">{w}</p>
					{/each}
				{:else}
					<div class="flex items-center gap-3">
						<h2 class="card-title my-0 flex-1">Mount</h2>
						<button class="btn btn-sm" onclick={() => send({ type: 'calibrate', action: 'start' })}>Calibrate</button>
					</div>
				{/if}
				{#if mount}
					<dl class="grid grid-cols-3 gap-2">
						<div class="fact"><dt class="caption">From plate</dt><dd>{mount.distance.toFixed(1)} ft</dd></div>
						<div class="fact"><dt class="caption">Height</dt><dd>{mount.height.toFixed(1)} ft</dd></div>
						<div class="fact"><dt class="caption">Lateral</dt><dd>{mount.lateral.toFixed(1)} ft</dd></div>
						{#if calibrating && mount.yaw != null && mount.pitch != null}
							<div class="fact"><dt class="caption">Yaw</dt><dd>{mount.yaw.toFixed(1)}°</dd></div>
							<div class="fact"><dt class="caption">Pitch</dt><dd>{mount.pitch.toFixed(1)}°</dd></div>
						{/if}
					</dl>
				{:else if !calibrating}
					<p class="opacity-70">Not calibrated.</p>
				{/if}
				{#if calibrating}
					<div class="mt-2 grid grid-cols-2 gap-2">
						<button class="btn btn-primary" disabled={!canApply} onclick={() => send({ type: 'calibrate', action: 'apply' })}>Apply</button>
						<button class="btn" onclick={() => send({ type: 'calibrate', action: 'stop' })}>Cancel</button>
						<button class="btn" disabled={!hasDots} onclick={undo}>Undo</button>
						<button class="btn" disabled={!hasDots} onclick={clear}>Clear</button>
					</div>
					<p class="opacity-70">
						{#if manual}
							Tap the five corners in order around the plate, from any corner, and drag a dot to move it.
						{:else}
							Tap the plate to pick it, and drag a dot if a corner is off. Clear to place the corners yourself.
						{/if}
						Green is the corners, yellow the fitted pose.
					</p>
				{:else}
					<p class="opacity-70">Once per camera placement, with the plate clear.</p>
				{/if}
			</div>
		</div>

		<div class="card surface">
			<div class="card-body">
				<h2 class="card-title my-0">Camera</h2>
				<div class="space-y-2">
					{#each steppers as [key, label, size, show] (key)}
						<Stepper {label} value={settings ? show(settings[key]) : '-'} step={show(size)} onstep={(d) => step(key, d * size)} />
					{/each}
				</div>
			</div>
		</div>
	</div>
</div>
