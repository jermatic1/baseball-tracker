<script lang="ts">
	import { live, send } from '#lib/socket.svelte.ts';
	import { getJson, sendJson } from '#lib/api.ts';
	import { sentence, type SummaryLine } from '#lib/format.ts';
	import Empty from '#lib/components/Empty.svelte';

	interface UserStats {
		name: string;
		sessions: number;
		hitting: SummaryLine[];
		pitching: SummaryLine[];
	}

	let name = $state('');
	let users = $state<UserStats[]>([]);
	// The user being renamed, and the new name.
	let renaming = $state<string | null>(null);
	let renameTo = $state('');
	// The user the delete dialog is about.
	let target = $state<UserStats | null>(null);
	let dialog = $state<HTMLDialogElement>();
	let busy = $state(false);

	// Every user's stats, again whenever the tracker's user list changes.
	$effect(() => {
		void live.users;
		getJson<{ users: UserStats[] }>('/api/history')
			.then((h) => (users = h.users))
			.catch(() => {});
	});

	const byActivity = (u: UserStats): [string, SummaryLine[]][] => [
		['hitting', u.hitting],
		['pitching', u.pitching]
	];

	function add() {
		const n = name.trim();
		if (!n) return;
		send({ type: 'add_user', name: n });
		name = '';
	}
	const idOf = (u: UserStats) => live.users.find((x) => x.name === u.name)?.id;
	async function call(method: string, url: string) {
		busy = true;
		try {
			await sendJson(method, url);
			return true;
		} catch (e) {
			live.error = (e as Error).message;
			return false;
		} finally {
			busy = false;
		}
	}
	async function rename(u: UserStats) {
		const id = idOf(u);
		const to = renameTo.trim();
		if (id == null || !to || to === u.name) {
			renaming = null;
			return;
		}
		busy = true;
		try {
			await sendJson('PUT', `/api/users/${id}`, { name: to });
			renaming = null;
		} catch (e) {
			live.error = (e as Error).message;
		} finally {
			busy = false;
		}
	}
	function confirmDelete(u: UserStats) {
		target = u;
		dialog?.showModal();
	}
	// Removes the user's sessions, and the user too unless only the
	// sessions were asked for.
	async function remove(keepUser: boolean) {
		const id = target && idOf(target);
		if (id == null) return;
		const url = keepUser ? `/api/users/${id}/sessions` : `/api/users/${id}?sessions=true`;
		if (await call('DELETE', url)) {
			dialog?.close();
			if (keepUser) users = (await getJson<{ users: UserStats[] }>('/api/history')).users;
		}
	}
</script>

<h1>Users</h1>

<div class="card surface mt-4">
	<div class="card-body">
		<form class="grid gap-4 sm:grid-cols-[1fr_auto] sm:items-end" onsubmit={(e) => { e.preventDefault(); add(); }}>
			<div>
				<div class="caption mb-1">New user</div>
				<input bind:value={name} placeholder="Name" class="input w-full" />
			</div>
			<button type="submit" class="btn btn-primary" disabled={!name.trim()}>Add</button>
		</form>
	</div>
</div>

<ul class="rows mt-4">
	{#each users as u (u.name)}
		<li class="flex flex-col gap-3 p-4 sm:flex-row sm:items-start">
			<div class="sm:w-48">
				{#if renaming === u.name}
					<form class="flex gap-2" onsubmit={(e) => { e.preventDefault(); rename(u); }}>
						<!-- svelte-ignore a11y_autofocus -->
						<input bind:value={renameTo} class="input input-sm min-w-0 flex-1" autofocus disabled={busy} />
						<button type="submit" class="btn btn-sm btn-primary" disabled={busy || !renameTo.trim()}>Save</button>
						<button type="button" class="btn btn-sm" onclick={() => (renaming = null)}>Cancel</button>
					</form>
				{:else}
					<div class="font-semibold">{u.name}</div>
				{/if}
				<div class="text-xs opacity-60">{u.sessions} {u.sessions === 1 ? 'session' : 'sessions'}</div>
			</div>
			<div class="grid flex-1 gap-3">
				{#each byActivity(u) as [activity, stats] (activity)}
					{#if stats.length}
						<div>
							<div class="text-sm font-medium capitalize">{activity}</div>
							<dl class="mt-2 grid grid-cols-2 gap-x-3 gap-y-2 sm:grid-cols-4">
								{#each stats as l (l.label)}
									<div class="fact"><dt class="caption">{sentence(l.label)}</dt><dd>{l.value.toLowerCase()}</dd></div>
								{/each}
							</dl>
						</div>
					{/if}
				{/each}
				{#if !u.hitting.length && !u.pitching.length}
					<div class="text-sm opacity-60">No sessions yet.</div>
				{/if}
			</div>
			<div class="flex gap-2 sm:flex-col sm:items-end">
				<button class="btn btn-sm" onclick={() => { renaming = u.name; renameTo = u.name; }}>Rename</button>
				<button class="btn btn-sm btn-error btn-soft" onclick={() => confirmDelete(u)}>Delete</button>
			</div>
		</li>
	{:else}
		<li><Empty title="No users yet">Add one above, then start a session on the <a href="/" class="link">Tracker</a> page.</Empty></li>
	{/each}
</ul>

<dialog bind:this={dialog} class="modal">
	<div class="modal-box">
		{#if target}
			<h3 class="text-lg font-semibold">Delete {target.name}?</h3>
			<p class="mt-2 opacity-70">
				{#if target.sessions}
					Their {target.sessions} {target.sessions === 1 ? 'session' : 'sessions'} and every clip and label in them go too. You can also keep the user and delete only the sessions.
				{:else}
					They have no sessions.
				{/if}
			</p>
			<div class="modal-action">
				<form method="dialog"><button class="btn" disabled={busy}>Cancel</button></form>
				{#if target.sessions}
					<button class="btn btn-error btn-soft" disabled={busy} onclick={() => remove(true)}>Delete sessions only</button>
				{/if}
				<button class="btn btn-error" disabled={busy} onclick={() => remove(false)}>
					{target.sessions ? 'Delete user and sessions' : 'Delete user'}
				</button>
			</div>
		{/if}
	</div>
	<form method="dialog" class="modal-backdrop"><button>close</button></form>
</dialog>
