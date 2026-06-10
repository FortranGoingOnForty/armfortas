! Sprint x05 curated program: IF/ELSE IF chains over integer relations.
! CHECK: 3
! CHECK: 12
program x05_if_chains
  implicit none
  integer :: v, cls, scaled
  v = 25
  if (v < 10) then
    cls = 1
  else if (v < 20) then
    cls = 2
  else if (v < 30) then
    cls = 3
  else
    cls = 4
  end if
  scaled = v / 2
  print *, cls
  print *, scaled
end program x05_if_chains
