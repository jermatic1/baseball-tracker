import tailwindcss from '@tailwindcss/vite';
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// The tracker serves the API and the websocket; in development Vite serves
// the app and proxies those to it.
const tracker = process.env.TRACKER ?? 'http://127.0.0.1:7880';

export default defineConfig({
	plugins: [
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			// A single-page app: the tracker serves index.html for any path it
			// does not know, and the app routes from there.
			adapter: adapter({ fallback: 'index.html' })
		})
	],
	server: {
		proxy: {
			'/api': tracker,
			'/ws': { target: tracker, ws: true },
			'^/sessions/[^/]+/[^/]+/api/': tracker
		}
	}
});
