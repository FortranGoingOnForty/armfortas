! CHECK: abc
! CHECK: label: value suffix
! CHECK: x           7 y
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar11_listdir_char_adjacent
  implicit none

  character(len=5) :: value
  value = 'value'

  print *, 'a', 'b', 'c'
  print *, 'label: ', trim(value), ' suffix'
  print *, 'x', 7, 'y'
end program ar11_listdir_char_adjacent
