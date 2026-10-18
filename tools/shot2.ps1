param([string]$Out = "C:\Users\WYT\Desktop\claudesktop\tools\shot2.png")
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
$proc = Get-Process claudesktop -ErrorAction Stop
$h = $proc.MainWindowHandle
Write-Output "handle=$h"
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
Write-Output "screen=$($b.Width)x$($b.Height)"
$bmp = [System.Drawing.Bitmap]::new($b.Width, $b.Height)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved $Out"
