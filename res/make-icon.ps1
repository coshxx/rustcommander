# Draws res\app.ico: white bold "RC" on a rounded square in the accent blue (#5b93ff),
# one PNG image per size. Run again after changing the design:
#   powershell -ExecutionPolicy Bypass -File res\make-icon.ps1

Add-Type -AssemblyName System.Drawing

$sizes = 16, 20, 24, 32, 40, 48, 64, 128, 256
$blue = [System.Drawing.Color]::FromArgb(255, 0x5b, 0x93, 0xff)
$out = Join-Path $PSScriptRoot 'app.ico'

function Draw-Png([int]$s) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.TextRenderingHint = 'AntiAliasGridFit'
    $g.Clear([System.Drawing.Color]::Transparent)

    # Rounded square filling the whole image; small sizes get relatively smaller corners.
    $r = [Math]::Max(2.0, $s * 0.2)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $path.AddArc(0, 0, $d, $d, 180, 90)
    $path.AddArc($s - $d, 0, $d, $d, 270, 90)
    $path.AddArc($s - $d, $s - $d, $d, $d, 0, 90)
    $path.AddArc(0, $s - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush $blue), $path)

    # "RC" as large as fits, centered by its measured outline (not by font metrics).
    $text = New-Object System.Drawing.Drawing2D.GraphicsPath
    $family = New-Object System.Drawing.FontFamily 'Segoe UI'
    $text.AddString('RC', $family, [int][System.Drawing.FontStyle]::Bold, 100, (New-Object System.Drawing.PointF 0, 0), [System.Drawing.StringFormat]::GenericTypographic)
    $b = $text.GetBounds()
    $target = $s * 0.78
    $k = $target / [Math]::Max($b.Width, $b.Height * 1.25)
    $m = New-Object System.Drawing.Drawing2D.Matrix
    $m.Translate(($s - $b.Width * $k) / 2, ($s - $b.Height * $k) / 2)
    $m.Scale($k, $k)
    $m.Translate(-$b.X, -$b.Y)
    $text.Transform($m)
    $g.FillPath([System.Drawing.Brushes]::White, $text)

    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    , $ms.ToArray()
}

# ICO: header, one 16-byte directory entry per image, then the PNG data.
$pngs = foreach ($s in $sizes) { , (Draw-Png $s) }
$fs = [System.IO.File]::Create($out)
$w = New-Object System.IO.BinaryWriter $fs
$w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]
    $dim = if ($s -ge 256) { 0 } else { $s }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([UInt16]1); $w.Write([UInt16]32)
    $w.Write([UInt32]$pngs[$i].Length); $w.Write([UInt32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $w.Write($p) }
$w.Close()
Write-Host "wrote $out"
