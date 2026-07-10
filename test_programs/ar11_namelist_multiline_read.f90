! CHECK: ios           0
! CHECK: values           7           1           2           3
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar11_namelist_multiline_read
  implicit none

  integer :: unit_num, ios
  integer :: n = 0
  integer :: arr(3) = 0
  namelist /cfg/ n, arr

  open(newunit=unit_num, file='ar11_namelist_multiline_read.nml', status='replace')
  write(unit_num, '(a)') '&cfg'
  write(unit_num, '(a)') ' n = 7'
  write(unit_num, '(a)') ' arr = 1, 2, 3'
  write(unit_num, '(a)') '/'
  close(unit_num)

  open(newunit=unit_num, file='ar11_namelist_multiline_read.nml', status='old')
  read(unit_num, nml=cfg, iostat=ios)
  close(unit_num, status='delete')

  print *, 'ios', ios
  print *, 'values', n, arr
end program ar11_namelist_multiline_read
