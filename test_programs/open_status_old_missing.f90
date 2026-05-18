! CHECK: ok
! REPRO_CHECK: run
program p
  implicit none
  integer :: unit_num, ios

  open(newunit=unit_num, file='open_status_old_missing.nope', &
       action='write', position='append', status='old', iostat=ios)
  if (ios == 0) then
    close(unit_num, status='delete')
    error stop 1
  end if

  open(newunit=unit_num, file='open_status_old_missing.nope', &
       action='read', position='asis', status='old', iostat=ios)
  if (ios == 0) then
    close(unit_num, status='delete')
    error stop 2
  end if

  print *, 'ok'
end program p
