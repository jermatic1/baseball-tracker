// One websocket to the tracker for the whole app. It retries on close, keeps
// what every page needs in `live`, and dispatches each message to the
// handlers pages register with `on`.
import { invalidateAll } from '$app/navigation';
import type { ModelStatus, SummaryLine } from './format';

export interface User {
	id: string;
	name: string;
}

export interface Session {
	id: number;
	user: string;
	activity: string;
	mode: string;
}

export interface Status {
	capturing: boolean;
	recording: boolean;
	calibrating: boolean;
	clients: string[];
	exposure: number;
	gain: number;
	ir_flood: number;
	ir_dot: number;
	mount: { distance: number; height: number; lateral: number };
	/** The strike zone at the front of the plate, in frame pixels. */
	zone?: number[][] | null;
	camera?: {
		left_rate: number;
		right_rate: number;
		pair_rate: number;
		left_gaps: number;
		right_gaps: number;
		unpaired: number;
	};
	detector?: {
		state: string;
		rate: number;
		provider: string;
		lag: number;
		drops: number;
		clips: number;
		last_event?: string;
		balls: LiveBall[];
	};
	model?: ModelStatus;
}

/** A ball in view: its box in frame pixels and, with depth, inches across the
 * plate, above the ground, and in front of the plate's front edge. */
export interface LiveBall {
	rect: number[];
	position?: number[] | null;
}

export interface Location {
	side: number;
	height: number;
}

/** The strike zone in inches: `width` across, `bottom` to `top` above the ground. */
export interface Zone {
	width: number;
	bottom: number;
	top: number;
}

export interface Pitch {
	release_velocity: number;
	plate_velocity: number;
	location: Location;
	simulated_location: Location;
	call: string;
	zone: Zone;
}

export interface HitRecord {
	kind: string;
	hit_type?: string;
	exit_velocity: number | null;
	launch_angle: number | null;
	spray_angle: number | null;
	pitch_velocity: number | null;
	hit?: { carry: number };
	pitch?: Pitch;
	contact?: { kind: string };
	samples: number;
}

export interface Event {
	/** Counts up per event received; a clip can carry several records. */
	seq: number;
	clip: string;
	record: HitRecord;
}

/** A clip recorded by hand, under its session id (`none` for the data directory). */
export interface Recorded {
	session: string;
	clip: string;
}

export interface SessionStats {
	session: number;
	pitches: number;
	strikes: number;
	avg_pitch_mph: number | null;
	top_pitch_mph: number | null;
	avg_exit_mph: number | null;
	top_exit_mph: number | null;
	longest_ft: number | null;
}

export interface SessionSummary {
	session: number;
	user: string;
	lines: SummaryLine[];
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Message = any;
type Handler = (m: Message) => void;

export type Link = 'connecting' | 'connected' | 'offline';

export const live = $state({
	connected: false,
	/** Whether the tracker has answered yet, so pages can tell a first load from an outage. */
	link: 'connecting' as Link,
	status: null as Status | null,
	session: null as Session | null,
	users: [] as User[],
	/** The newest first, at most twenty. */
	events: [] as Event[],
	/** The last clip recorded by hand, until the next one starts. */
	recorded: null as Recorded | null,
	stats: null as SessionStats | null,
	summary: null as SessionSummary | null,
	error: '',
	/** Each game module's latest body of each type, for a page opened later. */
	gameState: {} as Record<string, Record<string, Message>>
});

let ws: WebSocket | null = null;
let retry = 1000;
let timer: ReturnType<typeof setTimeout> | undefined;
let started = false;
let seq = 0;
const handlers = new Map<string, Set<Handler>>();

/** Register for a message type, `open`, `close`, or `blob`; returns the unregister. */
export function on(type: string, handler: Handler) {
	let set = handlers.get(type);
	if (!set) handlers.set(type, (set = new Set()));
	set.add(handler);
	return () => {
		set.delete(handler);
	};
}

function emit(type: string, m: Message) {
	handlers.get(type)?.forEach((h) => h(m));
}

export function send(msg: object) {
	if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

export const userName = (id: string) => live.users.find((u) => u.id === id)?.name ?? id;

/** Connect once; every open reloads the data the pages fetched over HTTP. */
export function start() {
	if (started) return;
	started = true;
	connect();
}

/** Try now instead of waiting out the backoff. */
export function reconnect() {
	clearTimeout(timer);
	retry = 1000;
	live.link = 'connecting';
	connect();
}

function connect() {
	const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
	ws = new WebSocket(`${scheme}://${location.host}/ws`);
	ws.binaryType = 'blob';
	ws.onopen = () => {
		retry = 1000;
		live.connected = true;
		live.link = 'connected';
		live.error = '';
		send({ type: 'hello', client: 'page' });
		invalidateAll();
		emit('open', null);
	};
	ws.onmessage = (e) => {
		if (e.data instanceof Blob) {
			emit('blob', e.data);
			return;
		}
		const m = JSON.parse(e.data);
		keep(m);
		emit(m.type, m);
	};
	ws.onclose = () => {
		live.connected = false;
		live.link = 'offline';
		emit('close', null);
		timer = setTimeout(connect, retry);
		retry = Math.min(retry * 2, 10000);
	};
}

function keep(m: Message) {
	switch (m.type) {
		case 'status':
			live.status = m;
			break;
		case 'users':
			live.users = m.users;
			break;
		case 'session':
			live.session = m.session;
			if (m.session) live.summary = null;
			live.stats = null;
			break;
		case 'event':
			live.events = [{ ...m, seq: ++seq }, ...live.events].slice(0, 20);
			break;
		case 'recorded':
			live.recorded = m;
			break;
		case 'session_stats':
			live.stats = m;
			break;
		case 'session_summary':
			live.summary = m;
			break;
		case 'error':
			live.error = m.request ? `${m.request}: ${m.message}` : m.message;
			break;
		case 'game': {
			const state = (live.gameState[m.game] ??= {});
			if (m.body?.type) state[m.body.type] = m.body;
			break;
		}
	}
}
