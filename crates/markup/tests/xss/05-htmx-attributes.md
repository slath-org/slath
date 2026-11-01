<div hx-get="/-/logout" hx-trigger="load">x</div>

<a data-hx-post="/-/settings/sessions/revoke-all" data-hx-trigger="load">y</a>

<button hx-on:click="alert(1)" hx-nonce="stolen">z</button>

[link](/-/logout "hx-post=x")
