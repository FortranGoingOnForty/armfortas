! ERROR_EXPECTED: list-directed format is not valid with REC=
program ar10_direct_list_directed_rejected
  implicit none

  open(30, file='ar10_direct_list_directed_rejected.dat', access='direct', &
       form='formatted', recl=16, status='replace')
  write(30, *, rec=1) 42
end program ar10_direct_list_directed_rejected
