$c = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=e2e-test-signer" -CertStoreLocation Cert:\LocalMachine\My
Export-Certificate -Cert $c -FilePath C:\e2e.cer | Out-Null
Import-Certificate -FilePath C:\e2e.cer -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
Import-Certificate -FilePath C:\e2e.cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
$r = Set-AuthenticodeSignature -FilePath C:\scream\Install\driver\x64\scream.cat -Certificate $c
"cat sign: $($r.Status)"
bcdedit /set testsigning on
