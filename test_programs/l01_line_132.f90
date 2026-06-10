! l01: free-form line over 132 chars under --std=f2018 — conformance
! warning fires, the line still compiles in full (acceptance never
! changes; F2023 raises the limit to 10,000).
! FLAGS: --std=f2018
! WARN_CHECK: characters long; F2018 limits free-form lines to 132
! CHECK: 70
program l01_line_132
  implicit none
  integer :: total
  total = 1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1
  print *, total
end program l01_line_132
