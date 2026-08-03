! DO CONCURRENT locality must preserve rank and pointer association.
! A LOCAL_INIT array receives an iteration-private value copy, a LOCAL array
! does not alias its outside storage, and a LOCAL_INIT pointer receives an
! iteration-private association that can be changed without changing the
! outside pointer.
!
! FLAGS: --std=f2023
! CHECK: 1 2 7 8 T
! CHECK: 111 222
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_allocate_like
! IR_CHECK: call @afs_copy_array_data_no_realloc
program ar43_do_concurrent_locality_array_pointer
  implicit none
  integer :: i
  integer, target :: a(2), b(2)
  integer, pointer :: p(:)
  integer :: copy(2), scratch(2), observed(2)

  a = [10, 20]
  b = [30, 40]
  p => a
  copy = [1, 2]
  scratch = [7, 8]
  observed = 0

  do concurrent (i = 1:2) local_init(copy, p) local(scratch) shared(observed, b) default(none)
    copy(i) = copy(i) + 100 * i
    scratch(i) = copy(i)
    observed(i) = scratch(i) + p(i)
    p => b
  end do

  print *, copy, scratch, associated(p, a)
  print *, observed
  if (any(copy /= [1, 2])) error stop 1
  if (any(scratch /= [7, 8])) error stop 2
  if (.not. associated(p, a)) error stop 3
  if (any(observed /= [111, 222])) error stop 4
end program ar43_do_concurrent_locality_array_pointer
