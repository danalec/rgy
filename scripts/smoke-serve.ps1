$dir = $args[0]
$queries = @(
    (@('needle', $dir) | ConvertTo-Json -Compress),
    (@('-c', 'needle', $dir) | ConvertTo-Json -Compress)
) -join "`n"
$queries | & 'C:\Users\danalec\Documents\src\rgx\target\release\rgx.exe' --serve
