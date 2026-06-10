! l01: pointer bounds remapping via an array expression (F2023
! 10.2.2.2) used to build a descriptor whose shape read 0. Until the
! remap lowering lands it must error loudly.
! FLAGS: --std=f2023
! ERROR_EXPECTED: pointer bounds remapping from an array expression
program l01_remap_reject
  implicit none
  integer, target :: t(6)
  integer, pointer :: q(:, :)
  t = [1, 2, 3, 4, 5, 6]
  q([2, 3]) => t
  print *, shape(q)
end program l01_remap_reject
