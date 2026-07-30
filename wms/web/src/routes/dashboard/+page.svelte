<!--
  The logistics map lives in the dashboard LAYOUT (`+layout.svelte` → DashboardMap),
  kept mounted across navigation so opening a ticket/repair doesn't tear it down and
  rebuild it. This route page is intentionally empty — the layout shows the map when
  the dashboard route is active and hides it (display:none, still mounted) otherwise.
-->
