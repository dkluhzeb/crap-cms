--- Regression fixture: a NON-auth collection with a legitimate field named
--- "password". Bulk create must persist it exactly like single create —
--- password separation only applies to auth collections.
crap.collections.define("wifi_networks", {
    labels = { singular = "WifiNetwork", plural = "WifiNetworks" },
    fields = {
        { name = "ssid", type = "text" },
        { name = "password", type = "text" },
    },
})
