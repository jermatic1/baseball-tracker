Use `task` for build, test, capture, review, camera checks, and release. Do not run ad-hoc shell for those.

Run `task check` before considering a change done. Do not leave compiler warnings. `task release` runs that check first.

The pages are the SvelteKit app in `web/`, styled with daisyUI components on Tailwind; the theme and the few project classes are in `web/src/routes/layout.css`. `task web` builds it, and `task screenshot` renders every page at phone and desktop widths into `dist/screenshots` for checking layouts.
