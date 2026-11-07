// Renders every page of the app at phone and desktop widths against the mock
// tracker, then the tracker page during a pitching session, between
// sessions, with the tracker gone, and with no detector yet, for checking
// layouts. Run from the repository root by
// `task screenshot`; output lands in dist/screenshots. CHROME names the
// browser executable when it is not on the path as chromium or chrome.
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import puppeteer from 'puppeteer-core';

const root = new URL('../..', import.meta.url).pathname;
const out = `${root}dist/screenshots`;
const base = 'http://127.0.0.1:7880';
const pages = [
	['tracker', '/'],
	['setup', '/setup'],
	['sessions', '/sessions'],
	['users', '/users'],
	['train', '/train'],
	['challenges', '/games/challenges'],
	['label', '/sessions/1/label'],
	['playback', '/sessions/1/playback']
];
const sizes = {
	phone: { width: 390, height: 844, deviceScaleFactor: 2, isMobile: true, hasTouch: true },
	desktop: { width: 1280, height: 800 }
};

function chrome() {
	if (process.env.CHROME) return process.env.CHROME;
	for (const name of ['chromium', 'chromium-browser', 'google-chrome', 'google-chrome-stable']) {
		const r = spawnSync('which', [name]);
		if (r.status === 0) return r.stdout.toString().trim();
	}
	throw new Error('no chromium or chrome on the path; set CHROME');
}

async function waitForTracker() {
	for (let i = 0; i < 60; i++) {
		try {
			if ((await fetch(`${base}/api/games`)).ok) return;
		} catch {
			/* not up yet */
		}
		await new Promise((r) => setTimeout(r, 500));
	}
	throw new Error('the mock tracker did not answer on port 7880');
}

// A page is ready once the header says the socket is up and a mock event or
// two has arrived; the preview stream keeps the network from ever going idle.
async function open(page, path) {
	await page.goto(base + path);
	await page.waitForSelector('header ::-p-text(connected)');
	await new Promise((r) => setTimeout(r, 3000));
}

async function shoot(page, name) {
	const file = `${out}/${name}.png`;
	await page.screenshot({ path: file, fullPage: true });
	console.log(file);
}

try {
	await fetch(`${base}/api/games`);
	throw new Error('something already answers on port 7880; stop it so the mock can take over');
} catch (e) {
	if (!(e instanceof TypeError)) throw e;
}

const startMock = (model) =>
	spawn(`${root}target/debug/tracker`, ['mock', 'data-mock', '--period', '2', '--model', model], {
		cwd: root,
		stdio: ['ignore', 'ignore', 'inherit']
	});

let mock = startMock('models/detector.onnx');
try {
	await waitForTracker();
	mkdirSync(out, { recursive: true });
	const browser = await puppeteer.launch({ executablePath: chrome(), args: ['--no-sandbox'] });
	try {
		// A session running, so the pages have something to show.
		const first = await browser.newPage();
		await first.setViewport(sizes.phone);
		await open(first, '/');
		const start = await first.$('::-p-text(Start session)');
		if (start) {
			await start.click();
			await first.waitForSelector('::-p-text(End session)');
		}
		await first.close();
		for (const [name, path] of pages) {
			for (const [size, viewport] of Object.entries(sizes)) {
				const page = await browser.newPage();
				await page.setViewport(viewport);
				await open(page, path);
				await shoot(page, `${name}-${size}`);
				await page.close();
			}
		}
		// The sessions page with one session opened for editing.
		const opened = await browser.newPage();
		await opened.setViewport(sizes.phone);
		await open(opened, '/sessions');
		await (await opened.waitForSelector('summary')).click();
		await new Promise((r) => setTimeout(r, 500));
		await shoot(opened, 'sessions-open-phone');
		await opened.close();
		// A pitching session with a few pitches in, for the strike zone plot.
		const pitching = await browser.newPage();
		await pitching.setViewport(sizes.desktop);
		await open(pitching, '/');
		await (await pitching.waitForSelector('::-p-text(End session)')).click();
		await (await pitching.waitForSelector('::-p-text(Pitching)')).click();
		await (await pitching.waitForSelector('::-p-text(Start session)')).click();
		await pitching.waitForSelector('::-p-text(Strike zone)', { timeout: 30000 });
		await new Promise((r) => setTimeout(r, 4500));
		await shoot(pitching, 'tracker-pitching-desktop');
		await pitching.close();
		const pitchingPhone = await browser.newPage();
		await pitchingPhone.setViewport(sizes.phone);
		await open(pitchingPhone, '/');
		await pitchingPhone.waitForSelector('::-p-text(Strike zone)');
		await shoot(pitchingPhone, 'tracker-pitching-phone');
		await pitchingPhone.close();
		// The start form once the session ends, then the page with no tracker.
		const page = await browser.newPage();
		await page.setViewport(sizes.phone);
		await open(page, '/');
		await (await page.waitForSelector('::-p-text(End session)')).click();
		await page.waitForSelector('::-p-text(Start session)');
		await new Promise((r) => setTimeout(r, 1000));
		await shoot(page, 'tracker-idle-phone');
		await (await page.waitForSelector('label[aria-label="menu"]')).click();
		await new Promise((r) => setTimeout(r, 500));
		await shoot(page, 'menu-phone');
		await page.click('.drawer-overlay');
		mock.kill('SIGINT');
		await page.waitForSelector('::-p-text(Not connected)');
		await shoot(page, 'offline-phone');
		await page.close();
		// A fresh install: no detector, so the tracker page offers to record.
		mock = startMock('models/missing.onnx');
		await waitForTracker();
		for (const [size, viewport] of Object.entries(sizes)) {
			const fresh = await browser.newPage();
			await fresh.setViewport(viewport);
			await open(fresh, '/');
			await shoot(fresh, `tracker-no-detector-${size}`);
			await fresh.close();
		}
	} finally {
		await browser.close();
	}
} finally {
	mock.kill('SIGINT');
}
