program ar4_namelist_array
  implicit none

  integer :: unit
  integer :: wios
  integer :: rios
  integer :: n = 7
  integer :: arr(3) = [1, 2, 3]

  namelist /cfg/ n, arr

  open(newunit=unit, file='ar4_namelist_array.out', status='replace', action='write')
  wios = -777
  write(unit, nml=cfg, iostat=wios)
  close(unit)

  print *, 'wios', wios
  ! CHECK: wios           0

  n = 0
  arr = 0
  open(newunit=unit, file='ar4_namelist_array.out', status='old', action='read')
  rios = -777
  read(unit, nml=cfg, iostat=rios)
  close(unit)

  print *, 'rios', rios
  ! CHECK: rios           0
  print *, 'values', n, arr
  ! CHECK: values           7           1           2           3

  ! FILE_CHECK: ar4_namelist_array.out => &CFG
  ! FILE_CHECK: ar4_namelist_array.out => N=7
  ! FILE_CHECK: ar4_namelist_array.out => ARR=1,2,3
end program
