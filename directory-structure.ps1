$languages = @("de", "en", "fr", "it", "es", "pt", "nl", "da", "sv", "fi", "el", "cs", "pl", "hu", "sk", "sl", "et", "lv", "lt", "ro", "bg", "hr", "ga", "mt", "ru", "tr", "ar", "zh", "uk", "sr", "he", "ko", "ja")
$namespaces = @("common", "rma", "dashboard", "auth")

New-Item -ItemType Directory -Path "locales" -Force

foreach ($lang in $languages) {
    New-Item -ItemType Directory -Path "locales\$lang" -Force
    foreach ($ns in $namespaces) {
        $file = "locales\$lang\$ns.json"
        if (!(Test-Path $file)) {
            "{}" | Out-File -Encoding utf8 -FilePath $file
        }
    }
}

Write-Host "done."
