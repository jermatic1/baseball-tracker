// JSON over HTTP to the tracker, with the response text as the error.

export async function getJson<T>(url: string): Promise<T> {
	const r = await fetch(url, { cache: 'no-store' });
	if (!r.ok) throw new Error(await r.text());
	return r.json();
}

export async function sendJson<T>(method: string, url: string, body?: unknown): Promise<T> {
	const r = await fetch(url, {
		method,
		headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
		body: body === undefined ? undefined : JSON.stringify(body)
	});
	if (!r.ok) throw new Error(await r.text());
	const text = await r.text();
	return (text ? JSON.parse(text) : undefined) as T;
}
